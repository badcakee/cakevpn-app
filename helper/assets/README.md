# Helper assets

`ads.srs` is the list behind "Block ads and trackers", built into the helper
so blocking works without downloading anything. `scripts/make-ad-list.sh`
makes it from two lists, each pinned to a commit:

- Hagezi's "Multi PRO mini" DNS blocklist (GPL-3.0),
  https://github.com/hagezi/dns-blocklists
- sing-box's compiled copy of v2fly's `category-ads-all`,
  https://github.com/SagerNet/sing-geosite

To update it, change the commits in the script and run it again.
