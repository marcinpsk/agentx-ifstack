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
        let mut by_index: BTreeMap<u32, &ObservedLink> = BTreeMap::new();
        let mut by_name: BTreeMap<&str, &ObservedLink> = BTreeMap::new();
        for link in &links {
            if link.index == 0 || link.index > i32::MAX as u32 {
                return Err(invalid(format!(
                    "interface {:?} has invalid index {}",
                    link.name, link.index
                )));
            }
            if link.name.is_empty() {
                return Err(invalid(format!(
                    "interface at index {} has an empty name",
                    link.index
                )));
            }
            if let Some(existing) = by_index.get(&link.index) {
                return Err(invalid(format!(
                    "interface {:?} duplicates index {} of interface {:?}",
                    link.name, link.index, existing.name
                )));
            }
            if let Some(existing) = by_name.get(link.name.as_str()) {
                return Err(invalid(format!(
                    "interface {:?} (index {}) duplicates the name of interface at index {}",
                    link.name, link.index, existing.index
                )));
            }
            by_index.insert(link.index, link);
            by_name.insert(link.name.as_str(), link);
        }

        let mut relationships = BTreeSet::new();
        for link in &links {
            if let Some(controller_index) = link.controller {
                let controller = by_index.get(&controller_index).ok_or_else(|| {
                    invalid(format!(
                        "interface {:?} (index {}) references unknown controller index {}",
                        link.name, link.index, controller_index
                    ))
                })?;
                if matches!(controller.kind, LinkKind::Bond | LinkKind::Bridge) {
                    if controller.index == link.index {
                        return Err(invalid(format!(
                            "interface {:?} (index {}) references itself as controller",
                            link.name, link.index
                        )));
                    }
                    relationships.insert(StackRelationship {
                        higher: controller.index,
                        lower: link.index,
                    });
                }
            }

            match link.kind {
                LinkKind::Vlan | LinkKind::MacVlan | LinkKind::IpVlan | LinkKind::MacVtap => {
                    let lower = link.lower.ok_or_else(|| {
                        invalid(format!(
                            "interface {:?} (index {}) has no lower sub-layer reference",
                            link.name, link.index
                        ))
                    })?;
                    if link.lower_netnsid.is_some() {
                        return Err(invalid(format!(
                            "interface {:?} (index {}) references lower sub-layer index {} in another network namespace",
                            link.name, link.index, lower
                        )));
                    }
                    if lower == link.index {
                        return Err(invalid(format!(
                            "interface {:?} (index {}) references itself as lower sub-layer",
                            link.name, link.index
                        )));
                    }
                    if !by_index.contains_key(&lower) {
                        return Err(invalid(format!(
                            "interface {:?} (index {}) references unknown lower sub-layer index {}",
                            link.name, link.index, lower
                        )));
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
                        if lower == link.index {
                            return Err(invalid(format!(
                                "interface {:?} (index {}) references itself as lower sub-layer",
                                link.name, link.index
                            )));
                        }
                        relationships.insert(StackRelationship {
                            higher: link.index,
                            lower,
                        });
                    }
                }
                _ => {}
            }
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

fn invalid(message: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidData, message.into())
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

    fn assert_rejected_with_message(links: Vec<ObservedLink>, expected: &str) {
        let error = Topology::from_observed(links).expect_err("accepted invalid inventory");

        assert_eq!(error.kind(), ErrorKind::InvalidData);
        assert_eq!(error.to_string(), expected);
    }

    #[test]
    fn invalid_index_error_names_the_interface_and_invalid_index() {
        for (link, expected) in [
            (
                ObservedLink::plain(0, "zero"),
                "interface \"zero\" has invalid index 0",
            ),
            (
                ObservedLink::plain(i32::MAX as u32 + 1, "large"),
                "interface \"large\" has invalid index 2147483648",
            ),
        ] {
            assert_rejected_with_message(vec![link], expected);
        }
    }

    #[test]
    fn invalid_name_error_names_the_interface_and_invalid_name() {
        assert_rejected_with_message(
            vec![ObservedLink::plain(7, "")],
            "interface at index 7 has an empty name",
        );
    }

    #[test]
    fn duplicate_index_error_names_the_interface_and_duplicate_index() {
        assert_rejected_with_message(
            vec![
                ObservedLink::plain(7, "first"),
                ObservedLink::plain(7, "second"),
            ],
            "interface \"second\" duplicates index 7 of interface \"first\"",
        );
    }

    #[test]
    fn duplicate_name_error_names_the_interface_and_duplicate_name() {
        assert_rejected_with_message(
            vec![
                ObservedLink::plain(7, "same"),
                ObservedLink::plain(8, "same"),
            ],
            "interface \"same\" (index 8) duplicates the name of interface at index 7",
        );
    }

    #[test]
    fn unknown_controller_error_names_the_interface_and_controller_index() {
        assert_rejected_with_message(
            vec![ObservedLink::plain(7, "member").with_controller(99)],
            "interface \"member\" (index 7) references unknown controller index 99",
        );
    }

    #[test]
    fn missing_lower_error_names_the_interface_and_missing_reference() {
        for (link, expected) in [
            (
                ObservedLink::of_kind(7, "vlan", LinkKind::Vlan),
                "interface \"vlan\" (index 7) has no lower sub-layer reference",
            ),
            (
                ObservedLink::of_kind(8, "macvlan", LinkKind::MacVlan),
                "interface \"macvlan\" (index 8) has no lower sub-layer reference",
            ),
            (
                ObservedLink::of_kind(9, "ipvlan", LinkKind::IpVlan),
                "interface \"ipvlan\" (index 9) has no lower sub-layer reference",
            ),
            (
                ObservedLink::of_kind(10, "macvtap", LinkKind::MacVtap),
                "interface \"macvtap\" (index 10) has no lower sub-layer reference",
            ),
        ] {
            assert_rejected_with_message(vec![link], expected);
        }
    }

    #[test]
    fn unknown_lower_error_names_the_interface_and_lower_index() {
        for (link, expected) in [
            (
                ObservedLink::of_kind(7, "vlan", LinkKind::Vlan).with_lower(99),
                "interface \"vlan\" (index 7) references unknown lower sub-layer index 99",
            ),
            (
                ObservedLink::of_kind(8, "macvlan", LinkKind::MacVlan).with_lower(98),
                "interface \"macvlan\" (index 8) references unknown lower sub-layer index 98",
            ),
            (
                ObservedLink::of_kind(9, "ipvlan", LinkKind::IpVlan).with_lower(97),
                "interface \"ipvlan\" (index 9) references unknown lower sub-layer index 97",
            ),
            (
                ObservedLink::of_kind(10, "macvtap", LinkKind::MacVtap).with_lower(96),
                "interface \"macvtap\" (index 10) references unknown lower sub-layer index 96",
            ),
        ] {
            assert_rejected_with_message(vec![link], expected);
        }
    }

    #[test]
    fn remote_lower_error_names_the_interface_and_lower_index() {
        for (link, expected) in [
            (
                ObservedLink::of_kind(7, "vlan", LinkKind::Vlan).with_remote_lower(99),
                "interface \"vlan\" (index 7) references lower sub-layer index 99 in another network namespace",
            ),
            (
                ObservedLink::of_kind(8, "macvlan", LinkKind::MacVlan).with_remote_lower(98),
                "interface \"macvlan\" (index 8) references lower sub-layer index 98 in another network namespace",
            ),
            (
                ObservedLink::of_kind(9, "ipvlan", LinkKind::IpVlan).with_remote_lower(97),
                "interface \"ipvlan\" (index 9) references lower sub-layer index 97 in another network namespace",
            ),
            (
                ObservedLink::of_kind(10, "macvtap", LinkKind::MacVtap).with_remote_lower(96),
                "interface \"macvtap\" (index 10) references lower sub-layer index 96 in another network namespace",
            ),
        ] {
            assert_rejected_with_message(vec![link], expected);
        }
    }

    #[test]
    fn self_stack_error_names_the_interface_and_self_reference() {
        for (link, expected) in [
            (
                ObservedLink::of_kind(7, "vlan", LinkKind::Vlan).with_lower(7),
                "interface \"vlan\" (index 7) references itself as lower sub-layer",
            ),
            (
                ObservedLink::of_kind(8, "macvlan", LinkKind::MacVlan).with_lower(8),
                "interface \"macvlan\" (index 8) references itself as lower sub-layer",
            ),
            (
                ObservedLink::of_kind(9, "ipvlan", LinkKind::IpVlan).with_lower(9),
                "interface \"ipvlan\" (index 9) references itself as lower sub-layer",
            ),
            (
                ObservedLink::of_kind(10, "macvtap", LinkKind::MacVtap).with_lower(10),
                "interface \"macvtap\" (index 10) references itself as lower sub-layer",
            ),
            (
                ObservedLink::of_kind(11, "vxlan", LinkKind::Vxlan).with_vxlan_lower(11),
                "interface \"vxlan\" (index 11) references itself as lower sub-layer",
            ),
            (
                ObservedLink::of_kind(12, "bond", LinkKind::Bond).with_controller(12),
                "interface \"bond\" (index 12) references itself as controller",
            ),
            (
                ObservedLink::of_kind(13, "bridge", LinkKind::Bridge).with_controller(13),
                "interface \"bridge\" (index 13) references itself as controller",
            ),
        ] {
            assert_rejected_with_message(vec![link], expected);
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
