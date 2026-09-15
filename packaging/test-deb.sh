#!/bin/sh
set -eu

package=$1
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
trap 'exit 1' HUP INT TERM
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends systemd man-db bsdextrautils lintian util-linux snmpd snmp
command -v col >/dev/null
lintian --fail-on error,warning "$package"
dpkg -i "$package"
agentx-ifstack --version
unit=/lib/systemd/system/agentx-ifstack.service
test -f "$unit"
systemd-analyze verify "$unit" 2>"$work/unit-warnings"
cat "$work/unit-warnings"
test ! -s "$work/unit-warnings"
test "$(systemctl is-enabled agentx-ifstack.service || true)" = disabled
test -f /usr/share/man/man8/agentx-ifstack.8.gz
man --warnings -E UTF-8 -l /usr/share/man/man8/agentx-ifstack.8.gz >/dev/null 2>"$work/man-warnings"
cat "$work/man-warnings"
test ! -s "$work/man-warnings"
for document in README.md LICENSE-MIT LICENSE-APACHE; do
    test -f "/usr/share/doc/agentx-ifstack/$document"
done
dpkg-query -W -f='${Conffiles}\n' agentx-ifstack | grep -F ' /etc/agentx-ifstack.toml '
sh /work/packaging/non-root-agentx.sh
printf '\n# Local configuration edit.\n' >> /etc/agentx-ifstack.toml
cp /etc/agentx-ifstack.toml "$work/expected-config"
dpkg -i "$package"
cmp "$work/expected-config" /etc/agentx-ifstack.toml
systemctl enable agentx-ifstack.service
apt-get remove -y agentx-ifstack
cmp "$work/expected-config" /etc/agentx-ifstack.toml
test ! -e /usr/bin/agentx-ifstack
test ! -e "$unit"
apt-get purge -y agentx-ifstack
test ! -e /etc/agentx-ifstack.toml
test ! -L /etc/systemd/system/multi-user.target.wants/agentx-ifstack.service
test ! -L /etc/systemd/system/agentx-ifstack.service
test ! -e /usr/share/man/man8/agentx-ifstack.8.gz
test ! -e /usr/share/doc/agentx-ifstack
