use std::collections::{BTreeMap, BTreeSet};
use std::io::{Error, ErrorKind, Result};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinkKind {
    Bond,
    Bridge,
    Vlan,
    MacVlan,
    IpVlan,
    MacVtap,
    Vxlan,
    Veth,
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservedLink {
    pub index: u32,
    pub name: String,
    pub kind: LinkKind,
    pub controller: Option<u32>,
    pub lower: Option<u32>,
    pub lower_netnsid: Option<i32>,
    pub vxlan_lower: Option<u32>,
}

impl ObservedLink {
    #[cfg(test)]
    pub(crate) fn plain(index: u32, name: &str) -> Self {
        Self::of_kind(index, name, LinkKind::Other)
    }

    #[cfg(test)]
    pub(crate) fn of_kind(index: u32, name: &str, kind: LinkKind) -> Self {
        Self {
            index,
            name: name.to_owned(),
            kind,
            controller: None,
            lower: None,
            lower_netnsid: None,
            vxlan_lower: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_controller(mut self, controller: u32) -> Self {
        self.controller = Some(controller);
        self
    }

    #[cfg(test)]
    fn with_lower(mut self, lower: u32) -> Self {
        self.lower = Some(lower);
        self
    }

    #[cfg(test)]
    fn with_remote_lower(mut self, lower: u32) -> Self {
        self.lower = Some(lower);
        self.lower_netnsid = Some(0);
        self
    }

    #[cfg(test)]
    fn with_vxlan_lower(mut self, lower: u32) -> Self {
        self.vxlan_lower = Some(lower);
        self
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct StackRelationship {
    pub higher: u32,
    pub lower: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Topology {
    interfaces: BTreeSet<u32>,
    relationships: BTreeSet<StackRelationship>,
}

impl Topology {
    pub fn from_observed(links: Vec<ObservedLink>) -> Result<Self> {
        let mut by_index = BTreeMap::new();
        let mut names = BTreeSet::new();
        for link in &links {
            if link.index == 0 || link.index > i32::MAX as u32 || link.name.is_empty() {
                return Err(invalid("invalid interface index or name"));
            }
            if by_index.insert(link.index, link).is_some() || !names.insert(link.name.as_str()) {
                return Err(invalid("duplicate interface index or name"));
            }
        }

        let mut relationships = BTreeSet::new();
        for link in &links {
            if let Some(controller) = link.controller {
                let controller = by_index
                    .get(&controller)
                    .ok_or_else(|| invalid("unknown controller interface"))?;
                if matches!(controller.kind, LinkKind::Bond | LinkKind::Bridge) {
                    relationships.insert(StackRelationship {
                        higher: controller.index,
                        lower: link.index,
                    });
                }
            }

            match link.kind {
                LinkKind::Vlan | LinkKind::MacVlan | LinkKind::IpVlan | LinkKind::MacVtap => {
                    if link.lower_netnsid.is_some() {
                        return Err(invalid("lower interface is in another network namespace"));
                    }
                    let lower = link
                        .lower
                        .ok_or_else(|| invalid("missing lower interface"))?;
                    if !by_index.contains_key(&lower) {
                        return Err(invalid("unknown lower interface"));
                    }
                    relationships.insert(StackRelationship {
                        higher: link.index,
                        lower,
                    });
                }
                LinkKind::Vxlan if link.lower_netnsid.is_none() => {
                    if let Some(lower) = link.vxlan_lower
                        && by_index.contains_key(&lower)
                    {
                        relationships.insert(StackRelationship {
                            higher: link.index,
                            lower,
                        });
                    }
                }
                _ => {}
            }
        }

        if relationships
            .iter()
            .any(|relationship| relationship.higher == relationship.lower)
        {
            return Err(invalid("interface cannot stack on itself"));
        }

        Ok(Self {
            interfaces: by_index.into_keys().collect(),
            relationships,
        })
    }

    pub fn interfaces(&self) -> &BTreeSet<u32> {
        &self.interfaces
    }

    pub fn relationships(&self) -> &BTreeSet<StackRelationship> {
        &self.relationships
    }
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn relationships(topology: &Topology) -> Vec<(u32, u32)> {
        topology
            .relationships()
            .iter()
            .map(|relationship| (relationship.higher, relationship.lower))
            .collect()
    }

    #[test]
    fn topology_keeps_direct_relationships_without_mib_boundary_rows() {
        let topology = Topology::from_observed(vec![
            ObservedLink::plain(3, "member").with_controller(9),
            ObservedLink::of_kind(9, "bond", LinkKind::Bond),
            ObservedLink::plain(10, "standalone"),
        ])
        .unwrap();

        assert_eq!(topology.interfaces(), &BTreeSet::from([3, 9, 10]));
        assert_eq!(
            topology.relationships(),
            &BTreeSet::from([StackRelationship {
                higher: 9,
                lower: 3,
            }])
        );
    }

    #[test]
    fn supported_links_preserve_higher_first_direct_relationships() {
        let mut links = vec![
            ObservedLink::plain(2, "lower"),
            ObservedLink::of_kind(9, "bond", LinkKind::Bond),
            ObservedLink::of_kind(10, "bridge", LinkKind::Bridge),
            ObservedLink::plain(3, "bond-member").with_controller(9),
            ObservedLink::plain(4, "bridge-member").with_controller(10),
        ];
        for (index, kind) in [
            (20, LinkKind::Vlan),
            (21, LinkKind::MacVlan),
            (22, LinkKind::IpVlan),
            (23, LinkKind::MacVtap),
        ] {
            links.push(ObservedLink::of_kind(index, &format!("link{index}"), kind).with_lower(2));
        }
        links.push(ObservedLink::of_kind(24, "vxlan", LinkKind::Vxlan).with_vxlan_lower(2));

        assert_eq!(
            relationships(&Topology::from_observed(links).unwrap()),
            [(9, 3), (10, 4), (20, 2), (21, 2), (22, 2), (23, 2), (24, 2),]
        );
    }

    #[test]
    fn veth_generic_links_and_unsupported_controllers_are_not_stack_relationships() {
        let topology = Topology::from_observed(vec![
            ObservedLink::plain(1, "lo"),
            ObservedLink::of_kind(2, "vrf", LinkKind::Other),
            ObservedLink::plain(3, "ens3").with_controller(2),
            ObservedLink::of_kind(4, "veth0", LinkKind::Veth).with_remote_lower(5),
            ObservedLink::of_kind(5, "veth1", LinkKind::Veth).with_lower(4),
        ])
        .unwrap();

        assert!(topology.relationships().is_empty());
    }

    #[test]
    fn unresolved_and_remote_vxlan_underlays_leave_vxlan_standalone() {
        for vxlan in [
            ObservedLink::of_kind(10, "vxlan", LinkKind::Vxlan),
            ObservedLink::of_kind(10, "vxlan", LinkKind::Vxlan).with_vxlan_lower(99),
            ObservedLink {
                lower_netnsid: Some(0),
                ..ObservedLink::of_kind(10, "vxlan", LinkKind::Vxlan).with_vxlan_lower(2)
            },
        ] {
            let topology =
                Topology::from_observed(vec![ObservedLink::plain(2, "lower"), vxlan]).unwrap();
            assert!(topology.relationships().is_empty());
        }
    }

    #[test]
    fn malformed_interfaces_and_supported_references_reject_the_inventory() {
        let invalid_cases = [
            vec![ObservedLink::plain(0, "zero")],
            vec![ObservedLink::plain(i32::MAX as u32 + 1, "large")],
            vec![ObservedLink::plain(1, "")],
            vec![ObservedLink::plain(1, "one"), ObservedLink::plain(1, "two")],
            vec![
                ObservedLink::plain(1, "same"),
                ObservedLink::plain(2, "same"),
            ],
            vec![ObservedLink::plain(1, "one").with_controller(99)],
            vec![ObservedLink::of_kind(1, "vlan", LinkKind::Vlan)],
            vec![
                ObservedLink::plain(1, "one"),
                ObservedLink::of_kind(2, "vlan", LinkKind::Vlan).with_lower(99),
            ],
            vec![
                ObservedLink::plain(1, "one"),
                ObservedLink::of_kind(2, "vlan", LinkKind::Vlan).with_remote_lower(1),
            ],
            vec![ObservedLink::of_kind(1, "vlan", LinkKind::Vlan).with_lower(1)],
            vec![ObservedLink::of_kind(1, "bond", LinkKind::Bond).with_controller(1)],
        ];

        for links in invalid_cases {
            assert!(
                Topology::from_observed(links.clone()).is_err(),
                "accepted {links:?}"
            );
        }
    }

    #[test]
    fn one_interface_can_be_both_a_higher_and_lower_layer() {
        let topology = Topology::from_observed(vec![
            ObservedLink::of_kind(2, "bond", LinkKind::Bond),
            ObservedLink::of_kind(3, "vlan", LinkKind::Vlan)
                .with_controller(2)
                .with_lower(2),
        ])
        .unwrap();

        assert_eq!(relationships(&topology), [(2, 3), (3, 2)]);
    }
}
