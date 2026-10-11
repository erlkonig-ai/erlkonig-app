#!/bin/sh
# Test-only failure injection. Never selected by the packager itself.
set -eu
case ${ERLKONIG_DMG_TEST_FAILURE:-}:$1 in
    create:create) printf 'test: injected create failure\n' >&2; exit 1 ;;
    after-attach:attach)
        /usr/bin/hdiutil "$@"
        printf 'test: injected failure after successful attach\n' >&2
        exit 1 ;;
esac
exec /usr/bin/hdiutil "$@"
