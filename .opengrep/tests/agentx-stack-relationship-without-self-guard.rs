// Fixture for agentx-stack-relationship-without-self-guard. Contains rule-violating
// code on purpose.

fn unguarded(peer: u32, link: &Link, relationships: &mut BTreeSet<StackRelationship>) {
    // ruleid: agentx-stack-relationship-without-self-guard
    relationships.insert(StackRelationship {
        higher: link.index,
        lower: peer,
    });
}

fn guard_that_only_logs(
    peer: u32,
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) {
    if link.index == peer {
        log::warn!("self reference");
    }
    // ruleid: agentx-stack-relationship-without-self-guard
    relationships.insert(StackRelationship {
        higher: link.index,
        lower: peer,
    });
}

fn reversed_guard_that_only_logs(
    peer: u32,
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) {
    if peer == link.index {
        log::warn!("self reference");
    }
    // ruleid: agentx-stack-relationship-without-self-guard
    relationships.insert(StackRelationship {
        higher: link.index,
        lower: peer,
    });
}

fn guard_that_rejects_after_inserting(
    peer: u32,
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) -> Result<()> {
    if link.index == peer {
        // ruleid: agentx-stack-relationship-without-self-guard
        relationships.insert(StackRelationship {
            higher: link.index,
            lower: peer,
        });
        return Err(invalid("self"));
    }
    Ok(())
}

fn guarded_with_bare_return(
    peer: u32,
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) {
    if link.index == peer {
        return;
    }
    // ok: agentx-stack-relationship-without-self-guard
    relationships.insert(StackRelationship {
        higher: link.index,
        lower: peer,
    });
}

fn reversed_guard_with_bare_return(
    peer: u32,
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) {
    if peer == link.index {
        return;
    }
    // ok: agentx-stack-relationship-without-self-guard
    relationships.insert(StackRelationship {
        higher: link.index,
        lower: peer,
    });
}

fn bare_return_after_logging(
    peer: u32,
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) {
    if link.index == peer {
        log::warn!("self reference");
        return;
    }
    // ok: agentx-stack-relationship-without-self-guard
    relationships.insert(StackRelationship {
        higher: link.index,
        lower: peer,
    });
}

fn guarded_after_logging(
    peer: u32,
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) -> Result<()> {
    if link.index == peer {
        log::warn!("self reference");
        return Err(invalid("self"));
    }
    // ok: agentx-stack-relationship-without-self-guard
    relationships.insert(StackRelationship {
        higher: link.index,
        lower: peer,
    });
    Ok(())
}

fn reversed_guard_after_logging(
    peer: u32,
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) -> Result<()> {
    if peer == link.index {
        log::warn!("self reference");
        return Err(invalid("self"));
    }
    // ok: agentx-stack-relationship-without-self-guard
    relationships.insert(StackRelationship {
        higher: link.index,
        lower: peer,
    });
    Ok(())
}

fn guarded_after_multiple_observations(
    peer: u32,
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) -> Result<()> {
    if link.index == peer {
        metrics::counter!("self_reference").increment(1);
        log::warn!("self reference");
        return Err(invalid("self"));
    }
    // ok: agentx-stack-relationship-without-self-guard
    relationships.insert(StackRelationship {
        higher: link.index,
        lower: peer,
    });
    Ok(())
}

fn guarded_with_continue(
    peers: &[u32],
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) {
    for peer in peers.iter().copied() {
        if link.index == peer {
            continue;
        }
        // ok: agentx-stack-relationship-without-self-guard
        relationships.insert(StackRelationship {
            higher: link.index,
            lower: peer,
        });
    }
}

fn reversed_guard_with_continue(
    peers: &[u32],
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) {
    for peer in peers.iter().copied() {
        if peer == link.index {
            continue;
        }
        // ok: agentx-stack-relationship-without-self-guard
        relationships.insert(StackRelationship {
            higher: link.index,
            lower: peer,
        });
    }
}

fn continue_after_logging(
    peers: &[u32],
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) {
    for peer in peers.iter().copied() {
        if link.index == peer {
            log::warn!("self reference");
            continue;
        }
        // ok: agentx-stack-relationship-without-self-guard
        relationships.insert(StackRelationship {
            higher: link.index,
            lower: peer,
        });
    }
}

fn conditionally_guarded_with_error(
    enabled: bool,
    peer: u32,
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) -> Result<()> {
    if link.index == peer {
        if enabled {
            return Err(invalid("self"));
        }
    }
    // ruleid: agentx-stack-relationship-without-self-guard
    relationships.insert(StackRelationship {
        higher: link.index,
        lower: peer,
    });
    Ok(())
}

fn conditionally_guarded_with_bare_return(
    enabled: bool,
    peer: u32,
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) {
    if peer == link.index {
        if enabled {
            return;
        }
    }
    // ruleid: agentx-stack-relationship-without-self-guard
    relationships.insert(StackRelationship {
        higher: link.index,
        lower: peer,
    });
}

fn conditionally_guarded_with_continue(
    enabled: bool,
    peers: &[u32],
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) {
    for peer in peers.iter().copied() {
        if link.index == peer {
            if enabled {
                continue;
            }
        }
        // ruleid: agentx-stack-relationship-without-self-guard
        relationships.insert(StackRelationship {
            higher: link.index,
            lower: peer,
        });
    }
}

fn logging_before_conditional_error(
    enabled: bool,
    peer: u32,
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) -> Result<()> {
    if link.index == peer {
        log::warn!("self reference");
        if enabled {
            return Err(invalid("self"));
        }
    }
    // ruleid: agentx-stack-relationship-without-self-guard
    relationships.insert(StackRelationship {
        higher: link.index,
        lower: peer,
    });
    Ok(())
}

fn logging_before_conditional_bare_return(
    enabled: bool,
    peer: u32,
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) {
    if link.index == peer {
        log::warn!("self reference");
        if enabled {
            return;
        }
    }
    // ruleid: agentx-stack-relationship-without-self-guard
    relationships.insert(StackRelationship {
        higher: link.index,
        lower: peer,
    });
}

fn logging_before_conditional_continue(
    enabled: bool,
    peers: &[u32],
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) {
    for peer in peers.iter().copied() {
        if link.index == peer {
            log::warn!("self reference");
            if enabled {
                continue;
            }
        }
        // ruleid: agentx-stack-relationship-without-self-guard
        relationships.insert(StackRelationship {
            higher: link.index,
            lower: peer,
        });
    }
}

fn guarded_with_shorthand_field(
    lower: u32,
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) -> Result<()> {
    if lower == link.index {
        return Err(invalid("self"));
    }
    // ok: agentx-stack-relationship-without-self-guard
    relationships.insert(StackRelationship {
        higher: link.index,
        lower,
    });
    Ok(())
}

// The guard may sit in an enclosing block, above the filter that reaches the insert.
fn guarded_from_an_enclosing_block(
    controller: &Link,
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) -> Result<()> {
    if controller.index == link.index {
        return Err(invalid("self"));
    }
    if matches!(controller.kind, LinkKind::Bond | LinkKind::Bridge) {
        // ok: agentx-stack-relationship-without-self-guard
        relationships.insert(StackRelationship {
            higher: controller.index,
            lower: link.index,
        });
    }
    Ok(())
}

// The known limit, asserted so it cannot change silently. This is the shape that shipped
// the defect: the guard exists, but the filter above it skips other link kinds, so a self
// reference never reaches the guard. The rule cannot see that, and does not match here.
// The exhaustive self reference test over every LinkKind in src/link.rs covers it.
fn guard_misplaced_inside_the_filter(
    controller: &Link,
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) -> Result<()> {
    if matches!(controller.kind, LinkKind::Bond | LinkKind::Bridge) {
        if controller.index == link.index {
            return Err(invalid("self"));
        }
        // ok: agentx-stack-relationship-without-self-guard
        relationships.insert(StackRelationship {
            higher: controller.index,
            lower: link.index,
        });
    }
    Ok(())
}

// Rust accepts the fields in either order, so the rule must see both. A reversed
// construction without a guard is the same defect as the forward one.
fn unguarded_with_reversed_field_order(
    peer: u32,
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) {
    // ruleid: agentx-stack-relationship-without-self-guard
    relationships.insert(StackRelationship {
        lower: peer,
        higher: link.index,
    });
}

fn unguarded_with_reversed_shorthand_fields(
    lower: u32,
    higher: u32,
    relationships: &mut BTreeSet<StackRelationship>,
) {
    // ruleid: agentx-stack-relationship-without-self-guard
    relationships.insert(StackRelationship { lower, higher });
}

fn reversed_field_order_inside_the_equality_branch(
    peer: u32,
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) -> Result<()> {
    if link.index == peer {
        // ruleid: agentx-stack-relationship-without-self-guard
        relationships.insert(StackRelationship {
            lower: peer,
            higher: link.index,
        });
        return Err(invalid("self"));
    }
    Ok(())
}

fn guarded_with_reversed_field_order(
    peer: u32,
    link: &Link,
    relationships: &mut BTreeSet<StackRelationship>,
) -> Result<()> {
    if link.index == peer {
        return Err(invalid("self"));
    }
    // ok: agentx-stack-relationship-without-self-guard
    relationships.insert(StackRelationship {
        lower: peer,
        higher: link.index,
    });
    Ok(())
}
