    function reject(message) {
        print "Non-root AgentX scenario failed: " message > "/dev/stderr"
        invalid = 1
    }

    BEGIN {
        while ((result = getline interface_index < expected_indexes) > 0) {
            if (interface_index !~ /^[1-9][0-9]*$/) {
                reject("invalid interface index from sysfs: " interface_index)
                continue
            }
            expected_interfaces[interface_index + 0] = 1
            expected_count++
        }
        close(expected_indexes)
        if (result < 0) {
            reject("could not read container interface indexes")
        }
        if (expected_count == 0) {
            reject("the container has no network interfaces")
        }
    }

    {
        oid = $1
        if (index(oid, prefix) != 1) {
            reject("walk returned a row outside ifStackStatus: " oid)
            next
        }
        suffix = substr(oid, length(prefix) + 1)
        components = split(suffix, indexes, ".")
        if (components != 2 || indexes[1] !~ /^[0-9]+$/ ||
                indexes[2] !~ /^[0-9]+$/) {
            reject("walk returned an invalid ifStackStatus instance: " oid)
            next
        }
        if ($2 != "=" || $3 != "INTEGER:" ||
                ($4 != "1" && $4 != "active(1)")) {
            reject("walk returned a non-active ifStackStatus value: " $0)
        }

        higher = indexes[1] + 0
        lower = indexes[2] + 0
        if (higher == 0 && lower == 0) {
            reject("walk returned the invalid boundary row 0.0")
        } else if (higher != 0 && lower != 0) {
            if (!(higher in expected_interfaces)) {
                reject("relationship row names an unknown interface: " higher)
            }
            if (!(lower in expected_interfaces)) {
                reject("relationship row names an unknown interface: " lower)
            }
            runs_over[higher] = 1
            covered_by[lower] = 1
        } else if (higher == 0) {
            if (!(lower in expected_interfaces)) {
                reject("zero-higher row names an unknown interface: " lower)
            }
            zero_higher[lower] = 1
        } else {
            if (!(higher in expected_interfaces)) {
                reject("zero-lower row names an unknown interface: " higher)
            }
            zero_lower[higher] = 1
        }

        if (rows > 0 && (higher < previous_higher ||
                (higher == previous_higher && lower <= previous_lower))) {
            reject("ifStackStatus rows are not in RFC index order at " suffix)
        }
        previous_higher = higher
        previous_lower = lower
        rows++
    }

    END {
        if (rows == 0) {
            reject("the ifStackTable walk returned no rows")
        }
        # RFC 2863 gives an interface a zero-higher row only when nothing runs
        # over it, and a zero-lower row only when it runs over nothing.
        for (index_value in expected_interfaces) {
            if (index_value in covered_by) {
                if (index_value in zero_higher) {
                    reject("unexpected zero-higher boundary row for interface " \
                        index_value)
                }
            } else if (!(index_value in zero_higher)) {
                reject("missing zero-higher boundary row for interface " index_value)
            }
            if (index_value in runs_over) {
                if (index_value in zero_lower) {
                    reject("unexpected zero-lower boundary row for interface " \
                        index_value)
                }
            } else if (!(index_value in zero_lower)) {
                reject("missing zero-lower boundary row for interface " index_value)
            }
        }
        if (invalid) {
            exit 1
        }
    }
