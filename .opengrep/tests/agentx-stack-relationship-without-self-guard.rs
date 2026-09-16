// Fixture for agentx-stack-relationship-without-self-guard. Contains rule-violating
// code on purpose.

fn unguarded(peer: u32, link: &Link, relationships: &mut BTreeSet<StackRelationship>) {
    // ruleid: agentx-stack-relationship-without-self-guard
    relationships.insert(StackRelationship {
        higher: link.index,
        lower: peer,
    });
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
