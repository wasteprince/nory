# World backdrop

Made with Natural Earth: public-domain physical land polygons at 1:110m.
No country borders or political claims are used.

Source: https://raw.githubusercontent.com/nvkelso/natural-earth-vector/ca96624a56bd078437bca8184e78163e5039ad19/geojson/ne_110m_land.geojson

Terms: https://www.naturalearthdata.com/about/terms-of-use/

`node scripts/render-map.mjs` converts the source to `world-land.bin`: a
240 × 120 full-world equirectangular bit mask, row-major with least-significant
bit first. Longitude runs west to east, latitude north to south. All continents,
including Antarctica, are retained. The application embeds only the 3,600-byte
mask; rendering needs no network, GeoJSON parser, or geographic calculations.

The map is centered and fitted inside the window with a fixed 2:1 aspect ratio.
