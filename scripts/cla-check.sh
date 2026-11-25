#!/bin/sh
# Says whether a pull request's author has signed the contributor licence agreement:
# whether CLA-SIGNERS.md, as the pull request leaves it, holds their line for this version.
# Usage: cla-check.sh LOGIN [SIGNERS_FILE]
set -eu

VERSION=1
# Accounts that sign nothing: the bots, and RUXN LLC's own.
EXEMPT='dependabot[bot] github-actions[bot] xnixy'

login=${1:?usage: cla-check.sh LOGIN [SIGNERS_FILE]}
signers=${2:-CLA-SIGNERS.md}

for name in $EXEMPT; do
    if [ "$login" = "$name" ]; then
        echo "$login signs nothing."
        exit 0
    fi
done

# A GitHub login is letters, digits and hyphens, so it is safe inside the pattern below.
case $login in
    *[!A-Za-z0-9-]* | '')
        echo "Not a login this check knows: $login" >&2
        exit 1
        ;;
esac

if grep -Eiq "^- .+, @$login, [0-9]{4}-[0-9]{2}-[0-9]{2}, version $VERSION\$" "$signers"; then
    echo "@$login has signed version $VERSION."
    exit 0
fi

cat >&2 <<MESSAGE
@$login hasn't signed the contributor licence agreement, version $VERSION.
Read CLA.md, then add your line to the end of CLA-SIGNERS.md in this pull request:

    - Your Name, @$login, $(date -u +%F), version $VERSION
MESSAGE
exit 1
