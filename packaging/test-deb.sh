#!/bin/sh
set -eu

package=$1
export DEBIAN_FRONTEND=noninteractive
apt-get update
apt-get install -y --no-install-recommends iproute2 systemd man-db bsdextrautils lintian
command -v col >/dev/null
lintian --fail-on error,warning "$package"
dpkg -i "$package"
agentx-ifstack --version
unit=/lib/systemd/system/agentx-ifstack.service
test -f "$unit"
systemd-analyze verify "$unit" 2>/tmp/unit-warnings
cat /tmp/unit-warnings
test ! -s /tmp/unit-warnings
test "$(systemctl is-enabled agentx-ifstack.service || true)" = disabled
test -f /usr/share/man/man8/agentx-ifstack.8.gz
man --warnings -E UTF-8 -l /usr/share/man/man8/agentx-ifstack.8.gz >/dev/null 2>/tmp/man-warnings
cat /tmp/man-warnings
test ! -s /tmp/man-warnings
for document in README.md LICENSE-MIT LICENSE-APACHE; do
    test -f "/usr/share/doc/agentx-ifstack/$document"
done
dpkg-query -W -f='${Conffiles}\n' agentx-ifstack | grep -F ' /etc/agentx-ifstack.toml '
printf '\n# Local configuration edit.\n' >> /etc/agentx-ifstack.toml
cp /etc/agentx-ifstack.toml /tmp/expected-config
dpkg -i "$package"
cmp /tmp/expected-config /etc/agentx-ifstack.toml
systemctl enable agentx-ifstack.service
apt-get remove -y agentx-ifstack
cmp /tmp/expected-config /etc/agentx-ifstack.toml
test ! -e /usr/bin/agentx-ifstack
test ! -e "$unit"
apt-get purge -y agentx-ifstack
test ! -e /etc/agentx-ifstack.toml
test ! -L /etc/systemd/system/multi-user.target.wants/agentx-ifstack.service
test ! -L /etc/systemd/system/agentx-ifstack.service
test ! -e /usr/share/man/man8/agentx-ifstack.8.gz
test ! -e /usr/share/doc/agentx-ifstack
