# Geosite Data

Place `dlc.dat` from `v2fly/domain-list-community` in this directory:

```text
data/dlc.dat
```

The binary `dlc.dat` file is intentionally ignored by Git. RProxy reads it at runtime when `routing.geosite.path` points here. If the file is missing or cannot be decoded, RProxy falls back to the small built-in seed list in `crates/rproxy-core/data/geosite-cn.txt`.

