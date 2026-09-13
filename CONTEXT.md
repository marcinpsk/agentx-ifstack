# Interface Stack

This context describes interface relationships exposed through IF-MIB
ifStackTable.

## Language

**Topology**:
The interfaces in the observed network namespace and their direct stack
relationships.

**Stack relationship**:
A direct relationship in which one interface runs over another interface.
An indirect path through other interfaces is not a stack relationship.
_Avoid_: Peer link

**Higher sub-layer**:
The interface that runs over the lower sub-layer in a stack relationship.
Its interface index is the first index in an ifStackStatus instance.

**Lower sub-layer**:
The interface over which the higher sub-layer runs in a stack relationship.
Its interface index is the second index in an ifStackStatus instance.

**Boundary row**:
An ifStackTable row with one zero index that represents a missing side of the
stack. A zero higher index means no interface runs over that interface; a zero
lower index means that interface runs over no other interface.
Boundary rows are table representations, not stack relationships.
