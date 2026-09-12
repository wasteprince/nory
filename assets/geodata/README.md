# GeoData used by NORY

NORY packages the pinned databases from [RoscomVPN GeoSite](https://github.com/hydraponique/roscomvpn-geosite) and [RoscomVPN GeoIP](https://github.com/hydraponique/roscomvpn-geoip). The exact release URLs and SHA-256 digests are in `scripts/tauri/geodata.py`.

RoscomVPN categories take precedence in both Xray and Mihomo, including `geosite:whitelist`, `geosite:torrent` and `geoip:direct`. Categories absent from RoscomVPN are retained from the Xray core archive for compatibility (for example `geoip:ru`). No category is renamed: `geoip:direct` and `geoip:ru` remain distinct lists.

The packaging script preserves each selected protobuf entry byte for byte, including domain attributes and inverse IP flags. A provenance manifest alongside the packaged licenses records the input and resulting database hashes and category counts. Updates to these data are delivered with NORY releases; cores do not replace them in the background.

GeoSite includes the upstream MIT license in `GeoSite-LICENSE`. RoscomVPN GeoIP publishes its custom lists without a separate license file; see its repository for source attribution. Xray archive licensing is included separately in the installer.
