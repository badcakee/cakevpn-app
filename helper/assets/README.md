# Helper assets

`geosite-category-ads-all.srs` is the ad and tracker list behind "Block ads and
trackers". It is sing-box's compiled copy of v2fly's `category-ads-all` list,
from https://github.com/SagerNet/sing-geosite (rule-set branch, commit
417ea33286a8c3a77fdce9b68ce25d58565dad43, 2026-09-30), and is built into the
helper so blocking works without downloading anything.

To update it, download the file from a newer commit of that branch and
replace this one.
