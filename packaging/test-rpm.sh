#!/bin/sh
set -eu

package=$1
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
trap 'exit 1' HUP INT TERM
dnf install -y iproute systemd rpm rpmlint man-db util-linux
command -v col >/dev/null
rpm -qpl "$package"
rpm -qpi "$package"
rpm -qp --scripts "$package"
url=$(rpm -qp --qf '%{URL}' "$package")
test -n "$url"
test "$url" != '(none)'
rpmlint -c /work/packaging/rpmlint.toml "$package"
rpm -i "$package"
agentx-ifstack --version
unit=/usr/lib/systemd/system/agentx-ifstack.service
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
rpm -qc agentx-ifstack | grep -Fx /etc/agentx-ifstack.toml
rpm -q --qf '[%{FILENAMES} %{FILEFLAGS:fflags}\n]' agentx-ifstack | grep -E '^/etc/agentx-ifstack.toml .*n'
cp /etc/agentx-ifstack.toml "$work/original-config"
printf '\n# Local configuration edit.\n' >> /etc/agentx-ifstack.toml
cp /etc/agentx-ifstack.toml "$work/expected-config"
rpm -U --replacepkgs "$package"
cmp "$work/expected-config" /etc/agentx-ifstack.toml
cp "$work/original-config" /etc/agentx-ifstack.toml
systemctl enable agentx-ifstack.service
rpm -e agentx-ifstack
test ! -e /usr/bin/agentx-ifstack
test ! -e "$unit"
test ! -e /etc/agentx-ifstack.toml
test ! -e /etc/agentx-ifstack.toml.rpmsave
test ! -L /etc/systemd/system/multi-user.target.wants/agentx-ifstack.service
test ! -e /usr/share/man/man8/agentx-ifstack.8.gz
test ! -e /usr/share/doc/agentx-ifstack/README.md
test ! -e /usr/share/doc/agentx-ifstack/LICENSE-MIT
test ! -e /usr/share/doc/agentx-ifstack/LICENSE-APACHE
test ! -e /usr/share/doc/agentx-ifstack
