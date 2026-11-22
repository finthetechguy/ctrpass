#!/bin/sh
# Installs ctrpass and the files that let the desktop app start its helper.
set -eu

usage() {
    cat <<USAGE
Usage: install.sh [options]

  --prefix DIR     Install under DIR (default /usr/local)
  --bin FILE       The ctrpass binary to install (default: the release build)
  --group NAME     Group allowed to start StreetPass without a password (default ctrpass)
  --user NAME      Add NAME to that group
  --no-rule        Always ask for a password
  --destdir DIR    Stage the files under DIR instead of the live system
  --uninstall      Remove what install.sh installed
USAGE
}

here=$(cd "$(dirname "$0")" && pwd)
prefix=/usr/local
bin="$here/../target/release/ctrpass"
group=ctrpass
user=
rule=1
destdir=
uninstall=0

while [ $# -gt 0 ]; do
    case "$1" in
        --prefix) prefix=$2; shift ;;
        --bin) bin=$2; shift ;;
        --group) group=$2; shift ;;
        --user) user=$2; shift ;;
        --no-rule) rule=0 ;;
        --destdir) destdir=$2; shift ;;
        --uninstall) uninstall=1 ;;
        -h|--help) usage; exit 0 ;;
        *) usage >&2; exit 2 ;;
    esac
    shift
done

case "$group" in
    *[!A-Za-z0-9_-]*|"") echo "Group names may use letters, digits, - and _." >&2; exit 2 ;;
esac
case "$prefix" in
    /*) ;;
    *) echo "The prefix must be an absolute path." >&2; exit 2 ;;
esac

id=io.github.finthetechguy.Ctrpass
bindir="$prefix/bin"
# polkit only reads actions and rules from these folders, whatever the prefix.
actions="$destdir/usr/share/polkit-1/actions"
rules="$destdir/etc/polkit-1/rules.d"
apps="$destdir$prefix/share/applications"

if [ "$uninstall" = 1 ]; then
    rm -f "$destdir$bindir/ctrpass" "$actions/$id.policy" "$rules/50-$id.rules" "$apps/$id.desktop"
    echo "Removed ctrpass. The $group group was kept."
    exit 0
fi

[ -x "$bin" ] || { echo "No ctrpass binary at $bin. Build it with: cargo build --release" >&2; exit 1; }

fill() {
    sed -e "s|@BINDIR@|$bindir|g" -e "s|@GROUP@|$group|g" "$1"
}

install -Dm755 "$bin" "$destdir$bindir/ctrpass"
mkdir -p "$actions" "$apps"
fill "$here/data/$id.policy.in" > "$actions/$id.policy"
chmod 644 "$actions/$id.policy"
install -m644 "$here/data/$id.desktop" "$apps/$id.desktop"

if [ "$rule" = 1 ]; then
    mkdir -p "$rules"
    fill "$here/data/$id.rules.in" > "$rules/50-$id.rules"
    chmod 644 "$rules/50-$id.rules"
    if [ -z "$destdir" ]; then
        getent group "$group" >/dev/null || groupadd --system "$group"
        if [ -n "$user" ]; then
            usermod -aG "$group" "$user"
            echo "Added $user to $group. Log out and back in for this to apply."
        fi
    fi
else
    rm -f "$rules/50-$id.rules"
fi

echo "Installed ctrpass to $bindir."
echo "Install the app with: pip install $here/../crates/ctrpass-py $here"
