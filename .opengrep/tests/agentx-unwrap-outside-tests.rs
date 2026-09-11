// Fixture for agentx-unwrap-outside-tests. Contains rule-violating code on purpose.

fn production(input: &str) -> u32 {
    // ruleid: agentx-unwrap-outside-tests
    input.parse::<u32>().unwrap()
}

fn production_ok(input: &str) -> Result<u32> {
    // ok: agentx-unwrap-outside-tests
    input.parse::<u32>().map_err(Error::other)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        // ok: agentx-unwrap-outside-tests
        assert_eq!(production_ok("7").unwrap(), 7);
    }
}

// A module literally named tests, but without #[cfg(test)], is production code.
mod outer {
    mod tests {
        fn helper(input: &str) -> u32 {
            // ruleid: agentx-unwrap-outside-tests
            input.parse::<u32>().unwrap()
        }
    }
}
