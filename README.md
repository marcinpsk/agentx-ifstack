# agentx-ifstack

An AgentX (RFC 2741) subagent that serves IF-MIB `ifStackTable`
(`1.3.6.1.2.1.31.1.2`) for Linux hosts, so SNMP monitoring systems can discover
interface relationships.

net-snmp implements neither `ifStackTable` nor `IEEE8023-LAG-MIB`, so bonds,
bridges and VLANs on a Linux host expose no stack rows today. This fills that
gap.

The implementation is proposed into this branch from `initial`. See that branch,
and its pull request, for the code and the review.

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
