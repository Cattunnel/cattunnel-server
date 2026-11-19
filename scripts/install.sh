#!/bin/sh

# CatTunnel server: no installer.
#
# The upstream version of this script downloaded upstream release archives.
# CatTunnel server is built from source, so this script only explains that
# and exits with an error. See README.md, sections "Build" and "Install and run".

cat >&2 <<'EOF'
This script does not install anything.

CatTunnel server is built from source:

    git clone https://github.com/Cattunnel/cattunnel-server.git
    cd cattunnel-server
    cargo build --bins --release

Then follow "Install and run" in README.md.
EOF
exit 1
