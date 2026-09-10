use std::collections::BTreeMap;
use std::ops::Bound;

use agentx::encodings::{ID, SearchRange, SearchRangeList, Value, VarBind, VarBindList};

pub const TABLE: [u32; 9] = [1, 3, 6, 1, 2, 1, 31, 1, 2];
const STATUS: [u32; 11] = [1, 3, 6, 1, 2, 1, 31, 1, 2, 1, 3];
const MAX_BULK_BINDINGS: usize = 4096;

pub struct Mib(BTreeMap<ID, Value>);

impl Mib {
    pub fn new(rows: Vec<(u32, u32)>) -> Self {
        Self(
            rows.into_iter()
                .map(|(higher, lower)| {
                    let mut components = STATUS.to_vec();
                    components.extend([higher, lower]);
                    (
                        ID::try_from(components).expect("fixed length OID"),
                        Value::Integer(1),
                    )
                })
                .collect(),
        )
    }

    pub fn get(&self, name: &ID) -> VarBind {
        let data = self.0.get(name).cloned().unwrap_or_else(|| {
            let status = ID::try_from(STATUS.to_vec()).expect("fixed length OID");
            let mut following = STATUS;
            following[10] += 1;
            let following = ID::try_from(following.to_vec()).expect("fixed length OID");
            if name >= &status && name < &following {
                Value::NoSuchInstance
            } else {
                Value::NoSuchObject
            }
        });
        binding(name, data)
    }

    pub fn get_next(&self, range: &SearchRange) -> VarBind {
        let start = if range.start.include == 1 {
            Bound::Included(&range.start)
        } else {
            Bound::Excluded(&range.start)
        };
        if let Some((name, data)) = self.0.range((start, Bound::Unbounded)).next()
            && (range.end.is_null() || name < &range.end)
        {
            return binding(name, data.clone());
        }
        binding(&range.start, Value::EndOfMibView)
    }

    pub fn get_bulk(
        &self,
        ranges: SearchRangeList,
        non_repeaters: u16,
        max_repetitions: u16,
    ) -> VarBindList {
        let count = usize::from(non_repeaters).min(ranges.len());
        let mut result: Vec<_> = ranges.0[..count]
            .iter()
            .take(MAX_BULK_BINDINGS)
            .map(|range| self.get_next(range))
            .collect();
        let mut repeaters = ranges.0[count..].to_vec();
        for _ in 0..max_repetitions {
            let mut exhausted = true;
            for range in &mut repeaters {
                if result.len() == MAX_BULK_BINDINGS {
                    return VarBindList(result);
                }
                let next = self.get_next(range);
                exhausted &= next.data == Value::EndOfMibView;
                range.start = next.name.clone();
                result.push(next);
            }
            if exhausted {
                break;
            }
        }
        VarBindList(result)
    }
}

fn binding(name: &ID, data: Value) -> VarBind {
    let mut name = name.clone();
    name.include = 0;
    VarBind::new(name, data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn oid(suffix: &str) -> ID {
        ID::from_str(&format!("1.3.6.1.2.1.31.1.2.1.3{suffix}")).unwrap()
    }

    #[test]
    fn get_distinguishes_missing_instances_and_objects() {
        let mib = Mib::new(vec![(2, 10)]);
        assert_eq!(mib.get(&oid(".2.10")).data, Value::Integer(1));
        for suffix in ["", ".2", ".2.11", ".2.10.0"] {
            assert_eq!(mib.get(&oid(suffix)).data, Value::NoSuchInstance);
        }
        assert_eq!(
            mib.get(&ID::try_from(TABLE.to_vec()).unwrap()).data,
            Value::NoSuchObject
        );
        assert_eq!(
            mib.get(&ID::from_str("1.3.6.1.2.1.31.1.2.1.30").unwrap())
                .data,
            Value::NoSuchObject
        );
    }

    #[test]
    fn walk_uses_numeric_components_and_terminates() {
        let mib = Mib::new(vec![(10, 2), (2, 10), (2, 2)]);
        let mut range = SearchRange::new(oid(""), ID::default());
        for suffix in [".2.2", ".2.10", ".10.2"] {
            let next = mib.get_next(&range);
            assert_eq!(next, VarBind::new(oid(suffix), Value::Integer(1)));
            assert!(next.name > range.start);
            range.start = next.name;
        }
        assert_eq!(
            mib.get_next(&range),
            VarBind::new(oid(".10.2"), Value::EndOfMibView)
        );
    }

    #[test]
    fn next_honors_inclusive_start_and_exclusive_end() {
        let mib = Mib::new(vec![(2, 2), (2, 10)]);
        let mut start = oid(".2.2");
        start.include = 1;
        let next = mib.get_next(&SearchRange::new(start.clone(), oid(".2.10")));
        assert_eq!(next.name, start);
        assert_eq!(next.name.include, 0);
        assert_eq!(next.data, Value::Integer(1));
        for (start, end) in [
            (oid(".2.2"), oid(".2.10")),
            (start.clone(), start),
            (oid(".9"), oid(".1")),
        ] {
            let result = mib.get_next(&SearchRange::new(start.clone(), end));
            assert_eq!(result, VarBind::new(start, Value::EndOfMibView));
        }
        let empty = Mib::new(vec![]);
        assert_eq!(
            empty.get_next(&SearchRange::default()).data,
            Value::EndOfMibView
        );
    }

    #[test]
    fn bulk_interleaves_repeaters_and_keeps_exhausted_names() {
        let mib = Mib::new(vec![(2, 2), (2, 10), (10, 2)]);
        let mut inclusive = oid(".2.2");
        inclusive.include = 1;
        let ranges = SearchRangeList(vec![
            SearchRange::new(oid(""), ID::default()),
            SearchRange::new(inclusive, oid(".10")),
            SearchRange::new(oid(".2.10"), ID::default()),
        ]);
        let result = mib.get_bulk(ranges.clone(), 1, 10).0;
        assert_eq!(
            result,
            vec![
                VarBind::new(oid(".2.2"), Value::Integer(1)),
                VarBind::new(oid(".2.2"), Value::Integer(1)),
                VarBind::new(oid(".10.2"), Value::Integer(1)),
                VarBind::new(oid(".2.10"), Value::Integer(1)),
                VarBind::new(oid(".10.2"), Value::EndOfMibView),
                VarBind::new(oid(".2.10"), Value::EndOfMibView),
                VarBind::new(oid(".10.2"), Value::EndOfMibView),
            ]
        );
        assert_eq!(mib.get_bulk(ranges.clone(), 1, 0).len(), 1);
        assert_eq!(mib.get_bulk(ranges, 20, 20).len(), 3);
        assert!(mib.get_bulk(SearchRangeList::default(), 0, 20).is_empty());
    }

    #[test]
    fn bulk_bounds_response_growth() {
        let mib = Mib::new((1..10000).map(|i| (i, 0)).collect());
        assert_eq!(
            mib.get_bulk(SearchRangeList(vec![SearchRange::default()]), 0, u16::MAX)
                .len(),
            MAX_BULK_BINDINGS
        );
    }
}
