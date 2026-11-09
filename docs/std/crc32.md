# crc32

CRC-32 checksums (IEEE 802.3: the one in zip, gzip and PNG). Catches
accidental corruption; not for security (use `crypto`).

```gojo
let c = crc32.checksum(data)
var c = 0                      // or piece by piece:
c = crc32.update(c, part1)
c = crc32.update(c, part2)
```

**Functions:** [`update`](#update), [`checksum`](#checksum)

## Functions

### update

```gojo
pub fn update(crc: u32, data: []byte) -> u32
```

The checksum `crc` (of earlier data; 0 to start) continued over `data`.

### checksum

```gojo
pub fn checksum(data: []byte) -> u32
```

The checksum of `data`.

