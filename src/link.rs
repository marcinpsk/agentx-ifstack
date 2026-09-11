use std::collections::{BTreeMap, BTreeSet};
use std::io::{Error, ErrorKind, Result};

use serde::Deserialize;

#[derive(Deserialize)]
struct Link {
    ifindex: u32,
    ifname: String,
    master: Option<String>,
    link: Option<String>,
    link_index: Option<u32>,
    link_netnsid: Option<i32>,
    linkinfo: Option<LinkInfo>,
}

#[derive(Deserialize)]
struct LinkInfo {
    info_kind: Option<String>,
    info_data: Option<InfoData>,
}

#[derive(Deserialize)]
struct InfoData {
    link: Option<Underlay>,
}

/// iproute2 prints the vxlan underlay as a name, and as an index when it cannot resolve one.
#[derive(Deserialize)]
#[serde(untagged)]
enum Underlay {
    Name(String),
    Index(u32),
}

impl Link {
    fn kind(&self) -> Option<&str> {
        self.linkinfo.as_ref()?.info_kind.as_deref()
    }
}

pub fn parse(input: &str) -> Result<Vec<(u32, u32)>> {
    let links: Vec<Link> =
        serde_json::from_str(input).map_err(|error| Error::new(ErrorKind::InvalidData, error))?;
    let mut names = BTreeMap::new();
    let mut indices = BTreeSet::new();
    for link in &links {
        if link.ifindex == 0 || link.ifindex > i32::MAX as u32 || link.ifname.is_empty() {
            return Err(invalid("invalid interface index or name"));
        }
        if names.insert(link.ifname.as_str(), link).is_some() || !indices.insert(link.ifindex) {
            return Err(invalid("duplicate interface index or name"));
        }
    }

    let mut rows = BTreeSet::new();
    for link in &links {
        if let Some(master) = &link.master {
            let higher = names
                .get(master.as_str())
                .ok_or_else(|| invalid("unknown master"))?;
            if matches!(higher.kind(), Some("bond" | "bridge")) {
                rows.insert((higher.ifindex, link.ifindex));
            }
        }
        if link.kind() == Some("vxlan") {
            // A vxlan reports its underlay in linkinfo.info_data, never in link or link_index.
            // iproute2 prints an index it cannot resolve as "if<index>", so an underlay that
            // names no local interface leaves the vxlan standalone instead of failing the table.
            if let Some(lower) = vxlan_lower(link, &names)
                && indices.contains(&lower)
            {
                rows.insert((link.ifindex, lower));
            }
        } else if matches!(link.kind(), Some("vlan" | "macvlan" | "ipvlan" | "macvtap")) {
            if link.link_netnsid.is_some() {
                return Err(invalid("lower interface is in another network namespace"));
            }
            let named = link
                .link
                .as_ref()
                .map(|name| {
                    names
                        .get(name.as_str())
                        .map(|lower| lower.ifindex)
                        .ok_or_else(|| invalid("unknown lower interface name"))
                })
                .transpose()?;
            let lower = match (named, link.link_index) {
                (Some(name_index), Some(index)) if name_index != index => {
                    return Err(invalid("conflicting lower interface references"));
                }
                (Some(index), _) | (None, Some(index)) => index,
                (None, None) => return Err(invalid("missing lower interface")),
            };
            if !indices.contains(&lower) {
                return Err(invalid("unknown lower interface index"));
            }
            rows.insert((link.ifindex, lower));
        }
    }
    if rows.iter().any(|(higher, lower)| higher == lower) {
        return Err(invalid("interface cannot stack on itself"));
    }
    let higher_layers: BTreeSet<_> = rows.iter().map(|&(higher, _)| higher).collect();
    let lower_layers: BTreeSet<_> = rows.iter().map(|&(_, lower)| lower).collect();
    for index in indices {
        if !lower_layers.contains(&index) {
            rows.insert((0, index));
        }
        if !higher_layers.contains(&index) {
            rows.insert((index, 0));
        }
    }
    Ok(rows.into_iter().collect())
}

fn vxlan_lower(link: &Link, names: &BTreeMap<&str, &Link>) -> Option<u32> {
    match link
        .linkinfo
        .as_ref()
        .and_then(|info| info.info_data.as_ref())
        .and_then(|data| data.link.as_ref())?
    {
        Underlay::Name(name) => names.get(name.as_str()).map(|lower| lower.ifindex),
        Underlay::Index(index) => Some(*index),
    }
}

fn invalid(message: &str) -> Error {
    Error::new(ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_runs_over_places_vlan_above_bond_and_bond_above_member() {
        let rows = parse(
            r#"[
            {"ifindex":3,"ifname":"nic2","master":"bond0"},
            {"ifindex":9,"ifname":"bond0","linkinfo":{"info_kind":"bond"}},
            {"ifindex":10,"ifname":"bond0.110","link":"bond0","linkinfo":{"info_kind":"vlan"}}
        ]"#,
        )
        .unwrap();
        let non_zero_relationships: Vec<_> = rows
            .iter()
            .copied()
            .filter(|&(higher, lower)| higher != 0 && lower != 0)
            .collect();
        // RFC 2863: x runs over y means ifStackStatus.x.y is active.
        assert_eq!(non_zero_relationships, [(9, 3), (10, 9)]);
        let boundary_rows: Vec<_> = rows
            .into_iter()
            .filter(|&(higher, lower)| higher == 0 || lower == 0)
            .collect();
        assert_eq!(boundary_rows, [(0, 10), (3, 0)]);
    }

    #[test]
    fn lower_interface_kinds_are_stack_layers_but_veth_peers_are_not() {
        for kind in ["macvlan", "ipvlan", "macvtap"] {
            for reference in [r#""link":"port2""#, r#""link_index":2"#] {
                let input = format!(
                    r#"[
                    {{"ifindex":2,"ifname":"port2"}},
                    {{"ifindex":10,"ifname":"port10",{reference},"linkinfo":{{"info_kind":"{kind}"}}}},
                    {{"ifindex":20,"ifname":"port20","link_index":21,"link_netnsid":0,"linkinfo":{{"info_kind":"veth"}}}},
                    {{"ifindex":21,"ifname":"port21","link":"port20","linkinfo":{{"info_kind":"veth"}}}}
                ]"#
                );
                assert_eq!(
                    parse(&input).unwrap(),
                    [(0, 10), (0, 20), (0, 21), (2, 0), (10, 2), (20, 0), (21, 0)],
                    "{kind} with {reference}"
                );
            }
        }
    }

    #[test]
    fn lower_interface_kinds_reject_invalid_references() {
        for kind in ["macvlan", "ipvlan", "macvtap"] {
            for reference in [
                r#""link_index":2,"link_netnsid":0"#,
                r#""link":"missing""#,
                r#""link_index":99"#,
                r#""link_index":10"#,
                r#""link":"port2","link_index":10"#,
                r#""link":null"#,
            ] {
                let input = format!(
                    r#"[
                    {{"ifindex":2,"ifname":"port2"}},
                    {{"ifindex":10,"ifname":"port10",{reference},"linkinfo":{{"info_kind":"{kind}"}}}}
                ]"#
                );
                assert!(parse(&input).is_err(), "accepted {kind} with {reference}");
            }
        }
    }

    #[test]
    fn topology_fixtures_separate_non_zero_relationships_and_boundary_rows() {
        type Rows = [(u32, u32)];
        let cases: &[(&str, &Rows, &Rows)] = &[
            (
                include_str!("../tests/fixtures/plain.json"),
                &[],
                &[(0, 1), (0, 2), (1, 0), (2, 0)],
            ),
            (
                include_str!("../tests/fixtures/bond.json"),
                &[(10, 2), (10, 3)],
                &[(0, 10), (2, 0), (3, 0)],
            ),
            (
                include_str!("../tests/fixtures/bridge.json"),
                &[(10, 2)],
                &[(0, 10), (2, 0)],
            ),
            (
                include_str!("../tests/fixtures/vlan.json"),
                &[(10, 2)],
                &[(0, 10), (2, 0)],
            ),
            (
                include_str!("../tests/fixtures/vlan_on_bond.json"),
                &[(10, 2), (20, 10)],
                &[(0, 20), (2, 0)],
            ),
            (
                include_str!("../tests/fixtures/bridge_vlan_bond.json"),
                &[(10, 2), (20, 10), (30, 10), (30, 20)],
                &[(0, 30), (2, 0)],
            ),
        ];
        for (input, expected_relationships, expected_boundary_rows) in cases {
            let mut reordered: Vec<serde_json::Value> = serde_json::from_str(input).unwrap();
            reordered.reverse();
            for input in [
                input.to_string(),
                serde_json::to_string(&reordered).unwrap(),
            ] {
                let (non_zero_relationships, boundary_rows): (Vec<_>, Vec<_>) = parse(&input)
                    .unwrap()
                    .into_iter()
                    .partition(|&(higher, lower)| higher != 0 && lower != 0);
                assert_eq!(non_zero_relationships, *expected_relationships);
                assert_eq!(boundary_rows, *expected_boundary_rows);
            }
        }
    }

    #[test]
    fn collected_proxmox_topology_has_only_direct_non_zero_relationships() {
        let rows = parse(include_str!("../tests/fixtures/proxmox.json")).unwrap();
        let non_zero_relationships: Vec<_> = rows
            .iter()
            .copied()
            .filter(|&(higher, lower)| higher != 0 && lower != 0)
            .collect();
        assert_eq!(
            non_zero_relationships,
            vec![
                (18, 2),
                (18, 4),
                (19, 18),
                (20, 19),
                (20, 157),
                (20, 165),
                (33, 13),
                (33, 15),
                (35, 33),
                (36, 35),
                (36, 169),
                (89, 33),
                (90, 89),
                (90, 161),
                (156, 155),
                (156, 158),
                (160, 159),
                (160, 162),
                (164, 163),
                (164, 166),
                (168, 167),
                (168, 170),
            ]
        );
    }

    #[test]
    fn collected_proxmox_topology_preserves_zero_index_boundary_rows() {
        let boundary_rows: Vec<_> = parse(include_str!("../tests/fixtures/proxmox.json"))
            .unwrap()
            .into_iter()
            .filter(|&(higher, lower)| higher == 0 || lower == 0)
            .collect();
        assert_eq!(
            boundary_rows,
            [
                (0, 1),
                (0, 3),
                (0, 5),
                (0, 6),
                (0, 7),
                (0, 8),
                (0, 9),
                (0, 10),
                (0, 11),
                (0, 12),
                (0, 14),
                (0, 16),
                (0, 17),
                (0, 20),
                (0, 36),
                (0, 90),
                (0, 156),
                (0, 160),
                (0, 164),
                (0, 168),
                (1, 0),
                (2, 0),
                (3, 0),
                (4, 0),
                (5, 0),
                (6, 0),
                (7, 0),
                (8, 0),
                (9, 0),
                (10, 0),
                (11, 0),
                (12, 0),
                (13, 0),
                (14, 0),
                (15, 0),
                (16, 0),
                (17, 0),
                (155, 0),
                (157, 0),
                (158, 0),
                (159, 0),
                (161, 0),
                (162, 0),
                (163, 0),
                (165, 0),
                (166, 0),
                (167, 0),
                (169, 0),
                (170, 0)
            ]
        );
    }

    #[test]
    fn invalid_topologies_fail_instead_of_becoming_empty() {
        for input in [
            "",
            "{}",
            "null",
            "[{}]",
            r#"[{"ifindex":0,"ifname":"port0"}]"#,
            r#"[{"ifindex":2147483648,"ifname":"port0"}]"#,
            r#"[{"ifindex":1,"ifname":""}]"#,
            r#"[{"ifindex":1,"ifname":"port1"},{"ifindex":1,"ifname":"port2"}]"#,
            r#"[{"ifindex":1,"ifname":"port1"},{"ifindex":2,"ifname":"port1"}]"#,
            r#"[{"ifindex":1,"ifname":"port1","master":2}]"#,
            r#"[{"ifindex":1,"ifname":"port1","master":"missing"}]"#,
            r#"[{"ifindex":1,"ifname":"port1","linkinfo":{"info_kind":"vlan"}}]"#,
            r#"[{"ifindex":1,"ifname":"port1","link_index":2,"linkinfo":{"info_kind":"vlan"}}]"#,
            r#"[{"ifindex":1,"ifname":"port1","link":"missing","linkinfo":{"info_kind":"vlan"}}]"#,
            r#"[{"ifindex":1,"ifname":"port1","link_index":1,"linkinfo":{"info_kind":"vlan"}}]"#,
            r#"[{"ifindex":1,"ifname":"port1","master":"port1","linkinfo":{"info_kind":"bond"}}]"#,
            r#"[{"ifindex":1,"ifname":"port1"},{"ifindex":2,"ifname":"port2","link":"port1","link_index":2,"linkinfo":{"info_kind":"vlan"}}]"#,
            r#"[{"ifindex":1,"ifname":"port1"},{"ifindex":2,"ifname":"port2","link_index":1,"link_netnsid":0,"linkinfo":{"info_kind":"vlan"}}]"#,
        ] {
            assert!(parse(input).is_err(), "accepted {input}");
        }
        assert_eq!(parse("[]").unwrap(), []);
    }

    #[test]
    fn veth_peer_indices_and_vrf_membership_are_not_stack_layers() {
        let input = r#"[
            {"ifindex":1,"ifname":"port1","linkinfo":{"info_kind":"vrf"}},
            {"ifindex":2,"ifname":"port2","master":"port1","link_index":2,"link_netnsid":0,"linkinfo":{"info_kind":"veth"}}
        ]"#;
        assert_eq!(parse(input).unwrap(), [(0, 1), (0, 2), (1, 0), (2, 0)]);
    }

    // Real `ip -details -json link show` puts the vxlan underlay in linkinfo.info_data,
    // never in the top-level link or link_index fields.
    #[test]
    fn a_vxlan_resolves_its_underlay_from_info_data() {
        let rows = parse(include_str!("../tests/fixtures/vxlan.json")).unwrap();
        let relationships: Vec<_> = rows
            .iter()
            .copied()
            .filter(|&(higher, lower)| higher != 0 && lower != 0)
            .collect();
        assert_eq!(relationships, [(10, 2)]);
    }

    #[test]
    fn a_vxlan_underlay_is_accepted_as_an_index() {
        let rows = parse(
            r#"[
            {"ifindex":2,"ifname":"port2"},
            {"ifindex":10,"ifname":"vx10","linkinfo":{"info_kind":"vxlan","info_data":{"link":2}}}
        ]"#,
        )
        .unwrap();
        assert_eq!(rows, [(0, 10), (2, 0), (10, 2)]);
    }

    #[test]
    fn a_vxlan_without_an_underlay_is_standalone() {
        let rows = parse(
            r#"[
            {"ifindex":10,"ifname":"vx10","linkinfo":{"info_kind":"vxlan","info_data":{"id":43}}}
        ]"#,
        )
        .unwrap();
        assert_eq!(rows, [(0, 10), (10, 0)]);
    }

    // iproute2 prints an underlay it cannot resolve as "if<index>", so an unresolvable
    // reference must drop the row rather than fail the whole table.
    #[test]
    fn a_vxlan_with_an_unresolvable_underlay_is_standalone() {
        for underlay in [r#""link":"if99""#, r#""link":"missing""#, r#""link":99"#] {
            let input = format!(
                r#"[
                {{"ifindex":2,"ifname":"port2"}},
                {{"ifindex":10,"ifname":"vx10","linkinfo":{{"info_kind":"vxlan","info_data":{{{underlay}}}}}}}
            ]"#
            );
            assert_eq!(
                parse(&input).unwrap(),
                [(0, 2), (0, 10), (2, 0), (10, 0)],
                "underlay {underlay}"
            );
        }
    }
}
