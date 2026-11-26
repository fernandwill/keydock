#!/usr/bin/env bash
# Builds dist/keydock-<tag>-x64.zip from target/release/keydock.exe and the pinned Interception release.
set -euo pipefail

tag=${1:?usage: package-release.sh <tag>}
interception_url=https://github.com/oblitum/Interception/releases/download/v1.0.1/Interception.zip
interception_sha256=ad038963d6413055765128b0b931f6e765147c9916dba79e65d872b261f9af10
# gnu.org is not reachable from every network, so take the GPL text from the SPDX list at a fixed commit.
gpl_url=https://raw.githubusercontent.com/spdx/license-list-data/cc4fe1ac462bd3ad3c81a07234baf7d9541ee871/text/GPL-3.0-only.txt
gpl_sha256=fb981668c18a279e285fc4d83fba1e836cc84dd4daa73c9697d3cfd2d8aca6e0

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

curl -sSfL --retry 3 -o "$work/Interception.zip" "$interception_url"
curl -sSfL --retry 3 -o "$work/GPL-3.0.txt" "$gpl_url"
(cd "$work" && printf '%s  Interception.zip\n%s  GPL-3.0.txt\n' "$interception_sha256" "$gpl_sha256" | sha256sum -c -)
unzip -q "$work/Interception.zip" -d "$work"

stage="$work/keydock-$tag-x64"
mkdir -p "$stage/licenses" dist
cp target/release/keydock.exe "$stage/"
cp LICENSE "$stage/licenses/keydock-MIT.txt"
cp "$work/Interception/library/x64/interception.dll" "$stage/"
cp "$work/Interception/command line installer/install-interception.exe" "$stage/"
cp "$work/Interception/licenses/non-commercial-usage/LGPL 3.0.txt" "$stage/licenses/Interception-LGPL-3.0.txt"
cp "$work/GPL-3.0.txt" "$stage/licenses/GPL-3.0.txt"
cat > "$stage/NOTICE.txt" <<'EOF'
keydock bundles Interception 1.0.1 by Francisco Lopes (https://github.com/oblitum/Interception)
under its non-commercial license:

- interception.dll is licensed under the GNU LGPL 3.0 (licenses/Interception-LGPL-3.0.txt,
  which builds on licenses/GPL-3.0.txt). keydock loads it at run time, so you can replace it
  with another build of the same library.
- install-interception.exe and the driver it installs are free for non-commercial use only.
  Commercial use needs a license from the author.

Install the driver once: open a terminal as administrator in this folder, run
    install-interception.exe /install
and restart Windows. To remove it, run install-interception.exe /uninstall and restart.
EOF

zip_path="dist/keydock-$tag-x64.zip"
rm -f "$zip_path"
# Windows' own bsdtar: Windows PowerShell's Compress-Archive writes backslash paths that break other unzip tools.
"$(cygpath -u "$SYSTEMROOT")/System32/tar.exe" -a -c -f "$(cygpath -w "$zip_path")" -C "$(cygpath -w "$work")" "keydock-$tag-x64"
echo "Built $zip_path"
