#!/bin/sh
# Exercise both download clients offline, including machines with wget only.
set -eu
installer=$(pwd)/site/install
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
mkdir "$tmp/bin"
for tool in sh mktemp rm awk grep head tr; do
    ln -s "$(command -v "$tool")" "$tmp/bin/$tool"
done

cat >"$tmp/client" <<'MOCK'
#!/bin/sh
set -eu
while [ "$#" -gt 0 ]; do
    case "$1" in
        -O|-o) output=$2; shift ;;
        -w) shift ;;
        https://*) url=$1 ;;
    esac
    shift
done
echo "$url" >>"$log"
case "$url" in
    */latest/download/*)
        [ "$scenario" = stable ] || exit 1
        echo 'echo installed-stable' >"$output" ;;
    */releases/latest)
        case "$scenario" in
            prerelease) status=404 ;;
            broken-asset) status=200 ;;
            outage) status=503 ;;
            offline) exit 1 ;;
        esac
        case "$0" in
            */curl) echo "$status" ;;
            */wget) echo "  HTTP/1.1 $status" >&2; [ "$status" = 200 ] ;;
        esac ;;
    *'?per_page=1')
        echo '[{"assets":[{"browser_download_url":"https://github.com/agom/figit/releases/download/v1-rc/figit-installer.sh"}]}]' >"$output" ;;
    */v1-rc/figit-installer.sh) echo 'echo installed-prerelease' >"$output" ;;
    *) exit 2 ;;
esac
MOCK
chmod +x "$tmp/client"
export log="$tmp/requests"
for client in curl wget; do
    ln -s "$tmp/client" "$tmp/bin/$client"
    for scenario in stable prerelease outage offline broken-asset; do
        export scenario
        : >"$log"
        if PATH="$tmp/bin" sh "$installer" >"$tmp/out" 2>"$tmp/error"; then
            case "$scenario" in
                stable|prerelease) grep -qx "installed-$scenario" "$tmp/out" ;;
                *) echo "unexpected success: $client $scenario" >&2; exit 1 ;;
            esac
        else
            case "$scenario" in
                stable|prerelease) cat "$tmp/error" >&2; exit 1 ;;
                *) grep -q 'retry' "$tmp/error"; test ! -s "$tmp/out" ;;
            esac
        fi
        if [ "$scenario" != prerelease ] && grep -q 'per_page' "$log"; then
            echo "unexpected prerelease lookup: $client $scenario" >&2
            exit 1
        fi
    done
    rm "$tmp/bin/$client"
done
