#!/usr/bin/env bash
# Builds helper/assets/ads.srs, the list behind "Block ads and trackers":
#   - Hagezi's "Multi PRO mini" DNS blocklist (ads, trackers, telemetry;
#     made to not break normal sites), https://github.com/hagezi/dns-blocklists
#   - sing-box's category-ads-all list, https://github.com/SagerNet/sing-geosite
# Both are pinned to a commit. To update the list, change the two commits
# below and run this again with a sing-box binary:
#
#   SING_BOX=/path/to/sing-box scripts/make-ad-list.sh
set -euo pipefail
HAGEZI=8167748054e27eb4ab435be10f09b2abf0e0c4bf
GEOSITE=417ea33286a8c3a77fdce9b68ce25d58565dad43
SING_BOX=${SING_BOX:-sing-box}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
WORK=$(mktemp -d)
trap 'rm -rf "$WORK"' EXIT

curl -fsSL -o "$WORK/hagezi.txt" "https://raw.githubusercontent.com/hagezi/dns-blocklists/$HAGEZI/wildcard/pro.mini-onlydomains.txt"
curl -fsSL -o "$WORK/geosite.srs" "https://raw.githubusercontent.com/SagerNet/sing-geosite/$GEOSITE/geosite-category-ads-all.srs"
"$SING_BOX" rule-set decompile -o "$WORK/geosite.json" "$WORK/geosite.srs"
python3 - "$WORK" <<'PY'
import json, re, sys
work = sys.argv[1]
geo = json.load(open(f"{work}/geosite.json"))["rules"][0]
ok = re.compile(r"^[a-z0-9]([a-z0-9.-]*[a-z0-9])?$")
suffixes = {d.strip() for d in open(f"{work}/hagezi.txt") if d.strip() and not d.startswith("#")}
suffixes |= set(geo.get("domain_suffix", []))
suffixes = sorted(d for d in suffixes if ok.match(d) and "." in d)
rule = {"domain": sorted(geo.get("domain", [])), "domain_suffix": suffixes}
if geo.get("domain_regex"):
    rule["domain_regex"] = geo["domain_regex"]
json.dump({"version": 2, "rules": [rule]}, open(f"{work}/ads.json", "w"))
print(len(suffixes), "domain suffixes,", len(rule["domain"]), "exact domains")
PY
"$SING_BOX" rule-set compile -o "$ROOT/helper/assets/ads.srs" "$WORK/ads.json"
ls -la "$ROOT/helper/assets/ads.srs"
