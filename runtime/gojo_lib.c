// ------------------------------------------------------------------ std helpers
//
// The parts of std/rand (and the other library modules) that are faster or
// only possible in C.

// rand: one xoshiro256** generator per thread, seeded from the operating
// system (or from GOJO_SEED, so a run can be replayed)
static __thread uint64_t rand_s[4];
static __thread bool rand_seeded;

static uint64_t rand_splitmix(uint64_t *x) {
    uint64_t z = (*x += 0x9E3779B97F4A7C15ULL);
    z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9ULL;
    z = (z ^ (z >> 27)) * 0x94D049BB133111EBULL;
    return z ^ (z >> 31);
}

static void rand_seed(void) {
    uint64_t k;
    const char *seed = getenv("GOJO_SEED");
    if (seed && *seed) k = strtoull(seed, NULL, 10) ^ 0x5851F42D4C957F2DULL;
    else if (getentropy(&k, sizeof k) != 0) k = (uint64_t)time(NULL) ^ (uint64_t)(uintptr_t)&k;
    for (int i = 0; i < 4; i++) rand_s[i] = rand_splitmix(&k);
    rand_seeded = true;
}

static inline uint64_t rand_rotl(uint64_t x, int k) { return (x << k) | (x >> (64 - k)); }

uint64_t gojo_rand___next(void) {
    if (__builtin_expect(!rand_seeded, 0)) rand_seed();
    uint64_t *s = rand_s;
    uint64_t r = rand_rotl(s[1] * 5, 7) * 9, t = s[1] << 17;
    s[2] ^= s[0];
    s[3] ^= s[1];
    s[1] ^= s[2];
    s[0] ^= s[3];
    s[2] ^= t;
    s[3] = rand_rotl(s[3], 45);
    return r;
}

// fills `n` bytes from the system's secure source; 0, or an errno
int64_t gojo_rand___secureFill(gojo_vec *v, int64_t n) {
    gojo_vec_reserve(v, n, 1);
    uint8_t *p = (uint8_t *)v->ptr + v->len;
    for (int64_t done = 0; done < n;) {
        // getentropy gives at most 256 bytes a call
        size_t k = n - done > 256 ? 256 : (size_t)(n - done);
        if (getentropy(p + done, k) != 0) return errno ? errno : EIO;
        done += (int64_t)k;
    }
    v->len += n;
    return 0;
}

// ------------------------------------------------------------------ encodings

static const char hex_lower[] = "0123456789abcdef", hex_upper[] = "0123456789ABCDEF";

static inline int hex_val(uint8_t c) {
    if (c >= '0' && c <= '9') return c - '0';
    if (c >= 'a' && c <= 'f') return c - 'a' + 10;
    if (c >= 'A' && c <= 'F') return c - 'A' + 10;
    return -1;
}

gojo_string gojo_hex___encode(gojo_slice d, bool upper) {
    gojo_string r = { NULL, 0, 0 };
    if (d.len == 0) return r;
    const char *dg = upper ? hex_upper : hex_lower;
    const uint8_t *p = d.ptr;
    r.ptr = gojo_alloc((size_t)d.len * 2);
    r.len = r.cap = d.len * 2;
    for (int64_t i = 0; i < d.len; i++) {
        r.ptr[2 * i] = (uint8_t)dg[p[i] >> 4];
        r.ptr[2 * i + 1] = (uint8_t)dg[p[i] & 15];
    }
    return r;
}

// -1 when it decoded, else the offset of the bad digit (the length for an odd count)
int64_t gojo_hex___decode(gojo_str s, gojo_vec *out) {
    for (int64_t i = 0; i < s.len; i++)
        if (hex_val(s.ptr[i]) < 0) return i;
    if (s.len & 1) return s.len;
    int64_t n = s.len / 2;
    if (n == 0) return -1;
    gojo_vec_reserve(out, n, 1);
    uint8_t *o = (uint8_t *)out->ptr + out->len;
    for (int64_t i = 0; i < n; i++) o[i] = (uint8_t)(hex_val(s.ptr[2 * i]) << 4 | hex_val(s.ptr[2 * i + 1]));
    out->len += n;
    return -1;
}

static const char b64_std[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
static const char b64_url[] = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

gojo_string gojo_base64___encode(gojo_slice d, bool url, bool pad) {
    gojo_string r = { NULL, 0, 0 };
    if (d.len == 0) return r;
    const char *a = url ? b64_url : b64_std;
    const uint8_t *p = d.ptr;
    int64_t full = d.len / 3, rem = d.len % 3;
    int64_t n = full * 4 + (rem ? (pad ? 4 : rem + 1) : 0);
    r.ptr = gojo_alloc((size_t)n);
    r.len = r.cap = n;
    uint8_t *o = r.ptr;
    for (int64_t i = 0; i < full; i++, p += 3, o += 4) {
        uint32_t v = (uint32_t)p[0] << 16 | (uint32_t)p[1] << 8 | p[2];
        o[0] = (uint8_t)a[v >> 18]; o[1] = (uint8_t)a[(v >> 12) & 63]; o[2] = (uint8_t)a[(v >> 6) & 63]; o[3] = (uint8_t)a[v & 63];
    }
    if (rem) {
        uint32_t v = (uint32_t)p[0] << 16 | (rem == 2 ? (uint32_t)p[1] << 8 : 0);
        o[0] = (uint8_t)a[v >> 18];
        o[1] = (uint8_t)a[(v >> 12) & 63];
        if (rem == 2) o[2] = (uint8_t)a[(v >> 6) & 63];
        else if (pad) o[2] = '=';
        if (pad) o[3] = '=';
    }
    return r;
}

static inline int b64_val(uint8_t c, bool url) {
    if (c >= 'A' && c <= 'Z') return c - 'A';
    if (c >= 'a' && c <= 'z') return c - 'a' + 26;
    if (c >= '0' && c <= '9') return c - '0' + 52;
    if (c == (url ? '-' : '+')) return 62;
    if (c == (url ? '_' : '/')) return 63;
    return -1;
}

// Padding is optional, but if present it must be right; the bits left over
// at the end must be 0 (so each text decodes from one encoding only).
// -1 when it decoded, else the offset of the problem.
int64_t gojo_base64___decode(gojo_str s, bool url, gojo_vec *out) {
    int64_t n = s.len;
    while (n > 0 && s.ptr[n - 1] == '=' && s.len - n < 2) n--;
    int64_t padn = s.len - n;
    if (n % 4 == 1) return n;
    // with padding, exactly what makes the length a multiple of 4
    if (padn && (n % 4 == 0 || padn != 4 - n % 4)) return n;
    for (int64_t i = 0; i < n; i++)
        if (b64_val(s.ptr[i], url) < 0) return i;
    int64_t full = n / 4, rem = n % 4, m = full * 3 + (rem ? rem - 1 : 0);
    if (m == 0) return -1;
    gojo_vec_reserve(out, m, 1);
    uint8_t *o = (uint8_t *)out->ptr + out->len;
    const uint8_t *p = s.ptr;
    for (int64_t i = 0; i < full; i++, p += 4, o += 3) {
        uint32_t v = (uint32_t)b64_val(p[0], url) << 18 | (uint32_t)b64_val(p[1], url) << 12 | (uint32_t)b64_val(p[2], url) << 6 | (uint32_t)b64_val(p[3], url);
        o[0] = (uint8_t)(v >> 16); o[1] = (uint8_t)(v >> 8); o[2] = (uint8_t)v;
    }
    if (rem) {
        uint32_t v = (uint32_t)b64_val(p[0], url) << 18 | (uint32_t)b64_val(p[1], url) << 12 | (rem == 3 ? (uint32_t)b64_val(p[2], url) << 6 : 0);
        if ((rem == 2 && (v & 0xFFFF)) || (rem == 3 && (v & 0xFF))) return full * 4 + rem - 1;
        o[0] = (uint8_t)(v >> 16);
        if (rem == 3) o[1] = (uint8_t)(v >> 8);
    }
    out->len += m;
    return -1;
}

// Percent-encoding. mode 0: a query component (space as `+`); 1: a path
// segment (`/` escaped); 2: a whole path (`/` kept); 3: user info. Unreserved characters
// (RFC 3986) are never escaped; paths also keep the sub-delimiters, `:` and `@`.
gojo_string gojo_url___escape(gojo_str s, int64_t mode) {
    gojo_string r = { NULL, 0, 0 };
    string_reserve(&r, s.len);
    for (int64_t i = 0; i < s.len; i++) {
        uint8_t c = s.ptr[i];
        bool keep = (c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '-' || c == '.' || c == '_' || c == '~';
        // (3: user info, where `@` must be escaped)
        if (!keep && mode > 0) keep = c != 0 && strchr(mode == 3 ? "!$&'()*+,;=:" : "!$&'()*+,;=:@", c) != NULL;
        if (!keep && mode == 2) keep = c == '/';
        if (c == ' ' && mode == 0) { gojo_string_push_byte(&r, '+'); continue; }
        if (keep) { gojo_string_push_byte(&r, c); continue; }
        gojo_string_push_byte(&r, '%');
        gojo_string_push_byte(&r, (uint8_t)hex_upper[c >> 4]);
        gojo_string_push_byte(&r, (uint8_t)hex_upper[c & 15]);
    }
    return r;
}

// `%XX` to bytes (and `+` to a space when `plus`). Strict: false on a `%`
// without two hex digits. Lenient: such a `%` is kept as it is.
bool gojo_url___unescape(gojo_str s, bool plus, bool lenient, gojo_string *out) {
    string_reserve(out, s.len);
    for (int64_t i = 0; i < s.len; i++) {
        uint8_t c = s.ptr[i];
        if (c == '%') {
            int h = i + 2 < s.len ? hex_val(s.ptr[i + 1]) : -1, l = h >= 0 ? hex_val(s.ptr[i + 2]) : -1;
            if (l < 0) {
                if (!lenient) return false;
                gojo_string_push_byte(out, c);
                continue;
            }
            gojo_string_push_byte(out, (uint8_t)(h << 4 | l));
            i += 2;
        } else {
            gojo_string_push_byte(out, plus && c == '+' ? ' ' : c);
        }
    }
    return true;
}

// ------------------------------------------------------------------ crypto
//
// MD5, SHA-1, SHA-256, SHA-384 and SHA-512 (FIPS 180-4, RFC 1321), streamed:
// a context lives in the bytes of a Gojo `Vec[byte]`. The constants were
// derived from the primes (and sines, for MD5) with exact integer arithmetic.

static const uint32_t sha256_k[64] = {
    0x428a2f98U, 0x71374491U, 0xb5c0fbcfU, 0xe9b5dba5U,
    0x3956c25bU, 0x59f111f1U, 0x923f82a4U, 0xab1c5ed5U,
    0xd807aa98U, 0x12835b01U, 0x243185beU, 0x550c7dc3U,
    0x72be5d74U, 0x80deb1feU, 0x9bdc06a7U, 0xc19bf174U,
    0xe49b69c1U, 0xefbe4786U, 0x0fc19dc6U, 0x240ca1ccU,
    0x2de92c6fU, 0x4a7484aaU, 0x5cb0a9dcU, 0x76f988daU,
    0x983e5152U, 0xa831c66dU, 0xb00327c8U, 0xbf597fc7U,
    0xc6e00bf3U, 0xd5a79147U, 0x06ca6351U, 0x14292967U,
    0x27b70a85U, 0x2e1b2138U, 0x4d2c6dfcU, 0x53380d13U,
    0x650a7354U, 0x766a0abbU, 0x81c2c92eU, 0x92722c85U,
    0xa2bfe8a1U, 0xa81a664bU, 0xc24b8b70U, 0xc76c51a3U,
    0xd192e819U, 0xd6990624U, 0xf40e3585U, 0x106aa070U,
    0x19a4c116U, 0x1e376c08U, 0x2748774cU, 0x34b0bcb5U,
    0x391c0cb3U, 0x4ed8aa4aU, 0x5b9cca4fU, 0x682e6ff3U,
    0x748f82eeU, 0x78a5636fU, 0x84c87814U, 0x8cc70208U,
    0x90befffaU, 0xa4506cebU, 0xbef9a3f7U, 0xc67178f2U,
};
static const uint32_t sha256_iv[8] = {
    0x6a09e667U, 0xbb67ae85U, 0x3c6ef372U, 0xa54ff53aU,
    0x510e527fU, 0x9b05688cU, 0x1f83d9abU, 0x5be0cd19U,
};
static const uint64_t sha512_k[80] = {
    0x428a2f98d728ae22ULL, 0x7137449123ef65cdULL, 0xb5c0fbcfec4d3b2fULL, 0xe9b5dba58189dbbcULL,
    0x3956c25bf348b538ULL, 0x59f111f1b605d019ULL, 0x923f82a4af194f9bULL, 0xab1c5ed5da6d8118ULL,
    0xd807aa98a3030242ULL, 0x12835b0145706fbeULL, 0x243185be4ee4b28cULL, 0x550c7dc3d5ffb4e2ULL,
    0x72be5d74f27b896fULL, 0x80deb1fe3b1696b1ULL, 0x9bdc06a725c71235ULL, 0xc19bf174cf692694ULL,
    0xe49b69c19ef14ad2ULL, 0xefbe4786384f25e3ULL, 0x0fc19dc68b8cd5b5ULL, 0x240ca1cc77ac9c65ULL,
    0x2de92c6f592b0275ULL, 0x4a7484aa6ea6e483ULL, 0x5cb0a9dcbd41fbd4ULL, 0x76f988da831153b5ULL,
    0x983e5152ee66dfabULL, 0xa831c66d2db43210ULL, 0xb00327c898fb213fULL, 0xbf597fc7beef0ee4ULL,
    0xc6e00bf33da88fc2ULL, 0xd5a79147930aa725ULL, 0x06ca6351e003826fULL, 0x142929670a0e6e70ULL,
    0x27b70a8546d22ffcULL, 0x2e1b21385c26c926ULL, 0x4d2c6dfc5ac42aedULL, 0x53380d139d95b3dfULL,
    0x650a73548baf63deULL, 0x766a0abb3c77b2a8ULL, 0x81c2c92e47edaee6ULL, 0x92722c851482353bULL,
    0xa2bfe8a14cf10364ULL, 0xa81a664bbc423001ULL, 0xc24b8b70d0f89791ULL, 0xc76c51a30654be30ULL,
    0xd192e819d6ef5218ULL, 0xd69906245565a910ULL, 0xf40e35855771202aULL, 0x106aa07032bbd1b8ULL,
    0x19a4c116b8d2d0c8ULL, 0x1e376c085141ab53ULL, 0x2748774cdf8eeb99ULL, 0x34b0bcb5e19b48a8ULL,
    0x391c0cb3c5c95a63ULL, 0x4ed8aa4ae3418acbULL, 0x5b9cca4f7763e373ULL, 0x682e6ff3d6b2b8a3ULL,
    0x748f82ee5defb2fcULL, 0x78a5636f43172f60ULL, 0x84c87814a1f0ab72ULL, 0x8cc702081a6439ecULL,
    0x90befffa23631e28ULL, 0xa4506cebde82bde9ULL, 0xbef9a3f7b2c67915ULL, 0xc67178f2e372532bULL,
    0xca273eceea26619cULL, 0xd186b8c721c0c207ULL, 0xeada7dd6cde0eb1eULL, 0xf57d4f7fee6ed178ULL,
    0x06f067aa72176fbaULL, 0x0a637dc5a2c898a6ULL, 0x113f9804bef90daeULL, 0x1b710b35131c471bULL,
    0x28db77f523047d84ULL, 0x32caab7b40c72493ULL, 0x3c9ebe0a15c9bebcULL, 0x431d67c49c100d4cULL,
    0x4cc5d4becb3e42b6ULL, 0x597f299cfc657e2aULL, 0x5fcb6fab3ad6faecULL, 0x6c44198c4a475817ULL,
};
static const uint64_t sha512_iv[8] = {
    0x6a09e667f3bcc908ULL, 0xbb67ae8584caa73bULL, 0x3c6ef372fe94f82bULL, 0xa54ff53a5f1d36f1ULL,
    0x510e527fade682d1ULL, 0x9b05688c2b3e6c1fULL, 0x1f83d9abfb41bd6bULL, 0x5be0cd19137e2179ULL,
};
static const uint64_t sha384_iv[8] = {
    0xcbbb9d5dc1059ed8ULL, 0x629a292a367cd507ULL, 0x9159015a3070dd17ULL, 0x152fecd8f70e5939ULL,
    0x67332667ffc00b31ULL, 0x8eb44a8768581511ULL, 0xdb0c2e0d64f98fa7ULL, 0x47b5481dbefa4fa4ULL,
};
static const uint32_t md5_t[64] = {
    0xd76aa478U, 0xe8c7b756U, 0x242070dbU, 0xc1bdceeeU,
    0xf57c0fafU, 0x4787c62aU, 0xa8304613U, 0xfd469501U,
    0x698098d8U, 0x8b44f7afU, 0xffff5bb1U, 0x895cd7beU,
    0x6b901122U, 0xfd987193U, 0xa679438eU, 0x49b40821U,
    0xf61e2562U, 0xc040b340U, 0x265e5a51U, 0xe9b6c7aaU,
    0xd62f105dU, 0x02441453U, 0xd8a1e681U, 0xe7d3fbc8U,
    0x21e1cde6U, 0xc33707d6U, 0xf4d50d87U, 0x455a14edU,
    0xa9e3e905U, 0xfcefa3f8U, 0x676f02d9U, 0x8d2a4c8aU,
    0xfffa3942U, 0x8771f681U, 0x6d9d6122U, 0xfde5380cU,
    0xa4beea44U, 0x4bdecfa9U, 0xf6bb4b60U, 0xbebfbc70U,
    0x289b7ec6U, 0xeaa127faU, 0xd4ef3085U, 0x04881d05U,
    0xd9d4d039U, 0xe6db99e5U, 0x1fa27cf8U, 0xc4ac5665U,
    0xf4292244U, 0x432aff97U, 0xab9423a7U, 0xfc93a039U,
    0x655b59c3U, 0x8f0ccc92U, 0xffeff47dU, 0x85845dd1U,
    0x6fa87e4fU, 0xfe2ce6e0U, 0xa3014314U, 0x4e0811a1U,
    0xf7537e82U, 0xbd3af235U, 0x2ad7d2bbU, 0xeb86d391U,
};

// algo: 0 MD5, 1 SHA-1, 2 SHA-256, 3 SHA-384, 4 SHA-512
typedef struct {
    int32_t algo;
    uint32_t n;      // bytes waiting in buf
    uint64_t len;    // bytes hashed so far
    union { uint32_t w[8]; uint64_t d[8]; } h;
    uint8_t buf[128];
} gojo_digest;

static inline uint32_t rol32(uint32_t x, int k) { return (x << k) | (x >> (32 - k)); }
static inline uint32_t ror32(uint32_t x, int k) { return (x >> k) | (x << (32 - k)); }
static inline uint64_t ror64(uint64_t x, int k) { return (x >> k) | (x << (64 - k)); }
static inline uint32_t be32(const uint8_t *p) { return (uint32_t)p[0] << 24 | (uint32_t)p[1] << 16 | (uint32_t)p[2] << 8 | p[3]; }
static inline uint64_t be64(const uint8_t *p) { return (uint64_t)be32(p) << 32 | be32(p + 4); }
static inline uint32_t le32(const uint8_t *p) { return (uint32_t)p[3] << 24 | (uint32_t)p[2] << 16 | (uint32_t)p[1] << 8 | p[0]; }

static void md5_block(uint32_t *h, const uint8_t *p) {
    static const int s[64] = { 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
                                4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21 };
    uint32_t m[16], a = h[0], b = h[1], c = h[2], d = h[3];
    for (int i = 0; i < 16; i++) m[i] = le32(p + 4 * i);
    for (int i = 0; i < 64; i++) {
        uint32_t f;
        int g;
        if (i < 16) { f = (b & c) | (~b & d); g = i; }
        else if (i < 32) { f = (d & b) | (~d & c); g = (5 * i + 1) & 15; }
        else if (i < 48) { f = b ^ c ^ d; g = (3 * i + 5) & 15; }
        else { f = c ^ (b | ~d); g = (7 * i) & 15; }
        uint32_t t = d;
        d = c;
        c = b;
        b = b + rol32(a + f + md5_t[i] + m[g], s[i]);
        a = t;
    }
    h[0] += a; h[1] += b; h[2] += c; h[3] += d;
}

static void sha1_block(uint32_t *h, const uint8_t *p) {
    uint32_t w[80], a = h[0], b = h[1], c = h[2], d = h[3], e = h[4];
    for (int i = 0; i < 16; i++) w[i] = be32(p + 4 * i);
    for (int i = 16; i < 80; i++) w[i] = rol32(w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16], 1);
    for (int i = 0; i < 80; i++) {
        uint32_t f, k;
        if (i < 20) { f = (b & c) | (~b & d); k = 0x5A827999; }
        else if (i < 40) { f = b ^ c ^ d; k = 0x6ED9EBA1; }
        else if (i < 60) { f = (b & c) | (b & d) | (c & d); k = 0x8F1BBCDC; }
        else { f = b ^ c ^ d; k = 0xCA62C1D6; }
        uint32_t t = rol32(a, 5) + f + e + k + w[i];
        e = d; d = c; c = rol32(b, 30); b = a; a = t;
    }
    h[0] += a; h[1] += b; h[2] += c; h[3] += d; h[4] += e;
}

static void sha256_block(uint32_t *h, const uint8_t *p) {
    uint32_t w[64], v[8];
    for (int i = 0; i < 16; i++) w[i] = be32(p + 4 * i);
    for (int i = 16; i < 64; i++) {
        uint32_t s0 = ror32(w[i - 15], 7) ^ ror32(w[i - 15], 18) ^ (w[i - 15] >> 3);
        uint32_t s1 = ror32(w[i - 2], 17) ^ ror32(w[i - 2], 19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16] + s0 + w[i - 7] + s1;
    }
    memcpy(v, h, sizeof v);
    for (int i = 0; i < 64; i++) {
        uint32_t S1 = ror32(v[4], 6) ^ ror32(v[4], 11) ^ ror32(v[4], 25);
        uint32_t ch = (v[4] & v[5]) ^ (~v[4] & v[6]);
        uint32_t t1 = v[7] + S1 + ch + sha256_k[i] + w[i];
        uint32_t S0 = ror32(v[0], 2) ^ ror32(v[0], 13) ^ ror32(v[0], 22);
        uint32_t mj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
        uint32_t t2 = S0 + mj;
        v[7] = v[6]; v[6] = v[5]; v[5] = v[4]; v[4] = v[3] + t1;
        v[3] = v[2]; v[2] = v[1]; v[1] = v[0]; v[0] = t1 + t2;
    }
    for (int i = 0; i < 8; i++) h[i] += v[i];
}

static void sha512_block(uint64_t *h, const uint8_t *p) {
    uint64_t w[80], v[8];
    for (int i = 0; i < 16; i++) w[i] = be64(p + 8 * i);
    for (int i = 16; i < 80; i++) {
        uint64_t s0 = ror64(w[i - 15], 1) ^ ror64(w[i - 15], 8) ^ (w[i - 15] >> 7);
        uint64_t s1 = ror64(w[i - 2], 19) ^ ror64(w[i - 2], 61) ^ (w[i - 2] >> 6);
        w[i] = w[i - 16] + s0 + w[i - 7] + s1;
    }
    memcpy(v, h, sizeof v);
    for (int i = 0; i < 80; i++) {
        uint64_t S1 = ror64(v[4], 14) ^ ror64(v[4], 18) ^ ror64(v[4], 41);
        uint64_t ch = (v[4] & v[5]) ^ (~v[4] & v[6]);
        uint64_t t1 = v[7] + S1 + ch + sha512_k[i] + w[i];
        uint64_t S0 = ror64(v[0], 28) ^ ror64(v[0], 34) ^ ror64(v[0], 39);
        uint64_t mj = (v[0] & v[1]) ^ (v[0] & v[2]) ^ (v[1] & v[2]);
        v[7] = v[6]; v[6] = v[5]; v[5] = v[4]; v[4] = v[3] + t1;
        v[3] = v[2]; v[2] = v[1]; v[1] = v[0]; v[0] = t1 + S0 + mj;
    }
    for (int i = 0; i < 8; i++) h[i] += v[i];
}

static uint32_t digest_block_size(int algo) { return algo >= 3 ? 128 : 64; }

static void digest_block(gojo_digest *c, const uint8_t *p) {
    switch (c->algo) {
    case 0: md5_block(c->h.w, p); break;
    case 1: sha1_block(c->h.w, p); break;
    case 2: sha256_block(c->h.w, p); break;
    default: sha512_block(c->h.d, p); break;
    }
}

// The context lives in a Vec[byte], which Gojo copies (`clone`) as bytes: it
// is only read and written with memcpy, never through a gojo_digest pointer,
// so strict aliasing holds whatever the bytes went through.
void gojo_crypto___init(gojo_vec *st, int64_t algo) {
    gojo_vec_reserve(st, (int64_t)sizeof(gojo_digest), 1);
    st->len = sizeof(gojo_digest);
    gojo_digest ctx, *c = &ctx;
    memset(c, 0, sizeof *c);
    c->algo = (int32_t)algo;
    switch (algo) {
    case 0: c->h.w[0] = 0x67452301; c->h.w[1] = 0xefcdab89; c->h.w[2] = 0x98badcfe; c->h.w[3] = 0x10325476; break;
    case 1: c->h.w[0] = 0x67452301; c->h.w[1] = 0xEFCDAB89; c->h.w[2] = 0x98BADCFE; c->h.w[3] = 0x10325476; c->h.w[4] = 0xC3D2E1F0; break;
    case 2: memcpy(c->h.w, sha256_iv, sizeof sha256_iv); break;
    case 3: memcpy(c->h.d, sha384_iv, sizeof sha384_iv); break;
    default: memcpy(c->h.d, sha512_iv, sizeof sha512_iv); break;
    }
    memcpy(st->ptr, c, sizeof *c);
}

void gojo_crypto___update(gojo_vec *st, gojo_slice data) {
    gojo_digest ctx, *c = &ctx;
    memcpy(c, st->ptr, sizeof *c);
    const uint8_t *p = data.ptr;
    uint64_t n = (uint64_t)data.len, bs = digest_block_size(c->algo);
    c->len += n;
    if (c->n) {
        uint64_t k = bs - c->n < n ? bs - c->n : n;
        memcpy(c->buf + c->n, p, k);
        c->n += (uint32_t)k;
        p += k;
        n -= k;
        if (c->n < bs) {
            memcpy(st->ptr, c, sizeof *c);
            return;
        }
        digest_block(c, c->buf);
        c->n = 0;
    }
    for (; n >= bs; n -= bs, p += bs) digest_block(c, p);
    if (n) memcpy(c->buf, p, n);
    c->n = (uint32_t)n;
    memcpy(st->ptr, c, sizeof *c);
}

// pads, and appends the digest to `out`; the context is used up
void gojo_crypto___finish(gojo_vec *st, gojo_vec *out) {
    gojo_digest ctx, *c = &ctx;
    memcpy(c, st->ptr, sizeof *c);
    uint32_t bs = digest_block_size(c->algo), lenBytes = bs == 128 ? 16 : 8;
    uint64_t bits = c->len * 8;
    c->buf[c->n++] = 0x80;
    if (c->n > bs - lenBytes) {
        memset(c->buf + c->n, 0, bs - c->n);
        digest_block(c, c->buf);
        c->n = 0;
    }
    memset(c->buf + c->n, 0, bs - c->n);
    for (int i = 0; i < 8; i++) {
        uint8_t b = (uint8_t)(bits >> (8 * i));
        if (c->algo == 0) c->buf[bs - 8 + i] = b;  // MD5: little-endian
        else c->buf[bs - 1 - i] = b;
    }
    digest_block(c, c->buf);
    static const int sizes[5] = { 16, 20, 32, 48, 64 };
    int n = sizes[c->algo];
    gojo_vec_reserve(out, n, 1);
    uint8_t *o = (uint8_t *)out->ptr + out->len;
    for (int i = 0; i < n; i++) {
        if (c->algo == 0) o[i] = (uint8_t)(c->h.w[i / 4] >> (8 * (i % 4)));
        else if (c->algo <= 2) o[i] = (uint8_t)(c->h.w[i / 4] >> (24 - 8 * (i % 4)));
        else o[i] = (uint8_t)(c->h.d[i / 8] >> (56 - 8 * (i % 8)));
    }
    out->len += n;
}

bool gojo_crypto___equal(gojo_slice a, gojo_slice b) {
    if (a.len != b.len) return false;
    volatile uint8_t d = 0;
    const uint8_t *x = a.ptr, *y = b.ptr;
    for (int64_t i = 0; i < a.len; i++) d |= x[i] ^ y[i];
    return d == 0;
}

// CRC-32 (IEEE 802.3, the one in zip, gzip and PNG), a table per byte position
static uint32_t crc_tab[8][256];
static pthread_once_t crc_once = PTHREAD_ONCE_INIT;

static void crc_init(void) {
    for (uint32_t i = 0; i < 256; i++) {
        uint32_t c = i;
        for (int k = 0; k < 8; k++) c = c & 1 ? 0xEDB88320U ^ (c >> 1) : c >> 1;
        crc_tab[0][i] = c;
    }
    for (int t = 1; t < 8; t++)
        for (int i = 0; i < 256; i++) crc_tab[t][i] = (crc_tab[t - 1][i] >> 8) ^ crc_tab[0][crc_tab[t - 1][i] & 0xFF];
}

uint32_t gojo_crc32_update(uint32_t crc, gojo_slice data) {
    pthread_once(&crc_once, crc_init);
    const uint8_t *p = data.ptr;
    int64_t n = data.len;
    uint32_t c = ~crc;
    // slicing-by-8: eight bytes a step
    while (n >= 8) {
        uint32_t lo = c ^ le32(p), hi = le32(p + 4);
        c = crc_tab[7][lo & 0xFF] ^ crc_tab[6][(lo >> 8) & 0xFF] ^ crc_tab[5][(lo >> 16) & 0xFF] ^ crc_tab[4][lo >> 24] ^
            crc_tab[3][hi & 0xFF] ^ crc_tab[2][(hi >> 8) & 0xFF] ^ crc_tab[1][(hi >> 16) & 0xFF] ^ crc_tab[0][hi >> 24];
        p += 8;
        n -= 8;
    }
    while (n--) c = crc_tab[0][(c ^ *p++) & 0xFF] ^ (c >> 8);
    return ~c;
}

// ------------------------------------------------------------------ tls
//
// TLS through the system's OpenSSL (1.1 or 3), loaded with dlopen the first
// time it is needed, so programs that don't use TLS don't depend on it. The
// sockets are non-blocking: when OpenSSL wants to read or write, the task
// parks on the poller (wait_fd) like any other network wait. Each connection
// has a mutex held around every OpenSSL call (never while parked), so one task
// can read while another writes. Results: > 0 a handle; 0 a TLS failure,
// described in *msg; < 0 an errno.

static struct {
    bool ok;
    char why[256];
    const void *(*client_method)(void);
    const void *(*server_method)(void);
    void *(*ctx_new)(const void *);
    void (*ctx_free)(void *);
    int (*ctx_default_paths)(void *);
    void (*ctx_set_verify)(void *, int, void *);
    int (*ctx_load_verify)(void *, const char *, const char *);
    int (*ctx_use_chain)(void *, const char *);
    int (*ctx_use_key)(void *, const char *, int);
    int (*ctx_check_key)(const void *);
    long (*ctx_ctrl)(void *, int, long, void *);
    uint64_t (*ctx_set_options)(void *, uint64_t);
    void *(*ssl_new)(void *);
    void (*ssl_free)(void *);
    int (*set_fd)(void *, int);
    long (*ssl_ctrl)(void *, int, long, void *);
    int (*set1_host)(void *, const char *);
    void (*set_connect_state)(void *);
    void (*set_accept_state)(void *);
    int (*do_handshake)(void *);
    int (*ssl_read)(void *, void *, int);
    int (*ssl_write)(void *, const void *, int);
    int (*get_error)(const void *, int);
    int (*shutdown)(void *);
    long (*verify_result)(const void *);
    const char *(*verify_string)(long);
    unsigned long (*err_get)(void);
    unsigned long (*err_peek)(void);
    void (*err_string)(unsigned long, char *, size_t);
    void (*err_clear)(void);
} ossl;
static pthread_once_t ossl_once = PTHREAD_ONCE_INIT;
static void *ossl_client_ctx;
static pthread_mutex_t ossl_ctx_lock = PTHREAD_MUTEX_INITIALIZER;

static void ossl_load(void) {
    static const char *names[] = {
#if defined(__APPLE__)
        "/opt/homebrew/opt/openssl@3/lib/libssl.3.dylib", "/usr/local/opt/openssl@3/lib/libssl.3.dylib",
        "/opt/homebrew/opt/openssl@1.1/lib/libssl.1.1.dylib", "/usr/local/opt/openssl@1.1/lib/libssl.1.1.dylib", "libssl.3.dylib",
#else
        "libssl.so.3", "libssl.so.1.1", "libssl.so",
#endif
    };
    const char *env = getenv("GOJO_LIBSSL");
    void *h = env && *env ? dlopen(env, RTLD_NOW | RTLD_LOCAL) : NULL;
    for (size_t i = 0; !h && i < sizeof names / sizeof names[0]; i++) h = dlopen(names[i], RTLD_NOW | RTLD_LOCAL);
    if (!h) {
        snprintf(ossl.why, sizeof ossl.why, "TLS needs OpenSSL 1.1 or 3 (libssl), which wasn't found; install it, or set GOJO_LIBSSL to its path");
        return;
    }
#define SYM(field, name) do { void *p = dlsym(h, name); memcpy(&ossl.field, &p, sizeof p); } while (0)
    SYM(client_method, "TLS_client_method");
    SYM(server_method, "TLS_server_method");
    SYM(ctx_new, "SSL_CTX_new");
    SYM(ctx_free, "SSL_CTX_free");
    SYM(ctx_default_paths, "SSL_CTX_set_default_verify_paths");
    SYM(ctx_set_verify, "SSL_CTX_set_verify");
    SYM(ctx_load_verify, "SSL_CTX_load_verify_locations");
    SYM(ctx_use_chain, "SSL_CTX_use_certificate_chain_file");
    SYM(ctx_use_key, "SSL_CTX_use_PrivateKey_file");
    SYM(ctx_check_key, "SSL_CTX_check_private_key");
    SYM(ctx_ctrl, "SSL_CTX_ctrl");
    SYM(ctx_set_options, "SSL_CTX_set_options");
    SYM(ssl_new, "SSL_new");
    SYM(ssl_free, "SSL_free");
    SYM(set_fd, "SSL_set_fd");
    SYM(ssl_ctrl, "SSL_ctrl");
    SYM(set1_host, "SSL_set1_host");
    SYM(set_connect_state, "SSL_set_connect_state");
    SYM(set_accept_state, "SSL_set_accept_state");
    SYM(do_handshake, "SSL_do_handshake");
    SYM(ssl_read, "SSL_read");
    SYM(ssl_write, "SSL_write");
    SYM(get_error, "SSL_get_error");
    SYM(shutdown, "SSL_shutdown");
    SYM(verify_result, "SSL_get_verify_result");
    SYM(verify_string, "X509_verify_cert_error_string");
    SYM(err_get, "ERR_get_error");
    SYM(err_peek, "ERR_peek_error");
    SYM(err_string, "ERR_error_string_n");
    SYM(err_clear, "ERR_clear_error");
#undef SYM
    if (!ossl.client_method || !ossl.ctx_new || !ossl.ssl_new || !ossl.set1_host || !ossl.do_handshake || !ossl.err_get || !ossl.err_peek || !ossl.ctx_ctrl) {
        snprintf(ossl.why, sizeof ossl.why, "the OpenSSL found is too old: TLS needs OpenSSL 1.1 or later");
        return;
    }
    ossl.ok = true;
}

static bool ossl_ready(gojo_string *msg) {
    pthread_once(&ossl_once, ossl_load);
    if (!ossl.ok) gojo_fmt_cstr(msg, ossl.why);
    return ossl.ok;
}

// the first error on OpenSSL's queue, after `what`
static void ossl_fail(gojo_string *msg, const char *what) {
    char buf[256];
    unsigned long e = ossl.err_get();
    gojo_fmt_cstr(msg, what);
    if (e) {
        ossl.err_string(e, buf, sizeof buf);
        gojo_fmt_cstr(msg, ": ");
        gojo_fmt_cstr(msg, buf);
    }
    ossl.err_clear();
}

// TLS 1.2 at least; a peer that closes without close_notify is an end of
// stream, not an error (OpenSSL 3), as most HTTPS servers do that
static void ossl_ctx_defaults(void *ctx) {
    ossl.ctx_ctrl(ctx, 123 /* SSL_CTRL_SET_MIN_PROTO_VERSION */, 0x0303 /* TLS1_2_VERSION */, NULL);
    if (ossl.ctx_set_options) ossl.ctx_set_options(ctx, (uint64_t)1 << 7 /* SSL_OP_IGNORE_UNEXPECTED_EOF */);
}

typedef struct {
    void *ssl;
    int fd;
    pthread_mutex_t mu;
} gojo_tls;

static gojo_tls *tls_wrap(void *ssl, int fd) {
    gojo_tls *t = gojo_alloc(sizeof *t);
    t->ssl = ssl;
    t->fd = fd;
    pthread_mutex_init(&t->mu, NULL);
    return t;
}

// Runs `op` until it is done, parking while OpenSSL waits for the socket.
// > 0 (or 0 at the end of the stream for reads): done; -1: failed, see *msg;
// -2: cancelled.
enum { TLS_HANDSHAKE, TLS_READ, TLS_WRITE };
static int64_t tls_op(gojo_tls *t, int op, void *buf, int n, gojo_string *msg) {
    for (;;) {
        pthread_mutex_lock(&t->mu);
        ossl.err_clear();
        int r = op == TLS_HANDSHAKE ? ossl.do_handshake(t->ssl) : op == TLS_READ ? ossl.ssl_read(t->ssl, buf, n) : ossl.ssl_write(t->ssl, buf, n);
        int e = r > 0 ? 0 : ossl.get_error(t->ssl, r);
        int sys = errno;
        if (r > 0 || e == 6 /* SSL_ERROR_ZERO_RETURN */) {
            pthread_mutex_unlock(&t->mu);
            return r > 0 ? r : 0;
        }
        if (e == 2 /* SSL_ERROR_WANT_READ */ || e == 3 /* SSL_ERROR_WANT_WRITE */) {
            pthread_mutex_unlock(&t->mu);
            if (!wait_fd(t->fd, e == 3)) return -2;
            continue;
        }
        if (e == 5 /* SSL_ERROR_SYSCALL */ && ossl.err_peek() == 0) {
            pthread_mutex_unlock(&t->mu);
            // OpenSSL 1.1: the peer closed without close_notify
            if (op == TLS_READ && (r == 0 || sys == 0)) return 0;
            gojo_fmt_cstr(msg, sys ? strerror(sys) : "the connection closed during the TLS handshake");
            return -1;
        }
        long v = op == TLS_HANDSHAKE ? ossl.verify_result(t->ssl) : 0;
        if (v != 0) {
            gojo_fmt_cstr(msg, "certificate verify failed: ");
            gojo_fmt_cstr(msg, ossl.verify_string(v));
            ossl.err_clear();
        } else {
            ossl_fail(msg, op == TLS_HANDSHAKE ? "TLS handshake failed" : op == TLS_READ ? "TLS read failed" : "TLS write failed");
        }
        pthread_mutex_unlock(&t->mu);
        return -1;
    }
}

static bool is_ip_literal(const char *h) {
    unsigned char b[16];
    return inet_pton(AF_INET, h, b) == 1 || inet_pton(AF_INET6, h, b) == 1;
}

// A client connection over `fd`, handshake done. `name`: the name the
// certificate must be for (and the SNI); `ca`: a PEM file to trust instead of
// the system's roots ("" for those); `verify` false: no checks at all.
int64_t gojo_tls___client(int64_t fd, gojo_str name, gojo_str ca, bool verify, gojo_string *msg) {
    if (!ossl_ready(msg)) return 0;
    char host[256], cafile[4096];
    if (name.len >= (int64_t)sizeof host || ca.len >= (int64_t)sizeof cafile || (name.len && memchr(name.ptr, 0, (size_t)name.len)) || (ca.len && memchr(ca.ptr, 0, (size_t)ca.len))) {
        gojo_fmt_cstr(msg, "server name or CA file path too long");
        return 0;
    }
    if (name.len) memcpy(host, name.ptr, (size_t)name.len);
    host[name.len] = 0;
    if (ca.len) memcpy(cafile, ca.ptr, (size_t)ca.len);
    cafile[ca.len] = 0;
    void *ctx;
    bool shared = verify && ca.len == 0;
    if (shared) {
        // the system roots are loaded once and shared
        pthread_mutex_lock(&ossl_ctx_lock);
        if (!ossl_client_ctx) {
            void *c = ossl.ctx_new(ossl.client_method());
            if (c) {
                ossl_ctx_defaults(c);
                ossl.ctx_set_verify(c, 1 /* SSL_VERIFY_PEER */, NULL);
                ossl.ctx_default_paths(c);
            }
            ossl_client_ctx = c;
        }
        ctx = ossl_client_ctx;
        pthread_mutex_unlock(&ossl_ctx_lock);
        if (!ctx) {
            ossl_fail(msg, "cannot create a TLS context");
            return 0;
        }
    } else {
        ctx = ossl.ctx_new(ossl.client_method());
        if (!ctx) {
            ossl_fail(msg, "cannot create a TLS context");
            return 0;
        }
        ossl_ctx_defaults(ctx);
        if (verify) {
            ossl.ctx_set_verify(ctx, 1, NULL);
            if (ossl.ctx_load_verify(ctx, cafile, NULL) != 1) {
                ossl_fail(msg, "cannot load the CA file");
                ossl.ctx_free(ctx);
                return 0;
            }
        }
    }
    void *ssl = ossl.ssl_new(ctx);
    if (!shared) ossl.ctx_free(ctx); // the connection keeps its own reference
    if (!ssl) {
        ossl_fail(msg, "cannot create a TLS connection");
        return 0;
    }
    ossl.set_fd(ssl, (int)fd);
    ossl.set_connect_state(ssl);
    if (name.len && !is_ip_literal(host)) ossl.ssl_ctrl(ssl, 55 /* SSL_CTRL_SET_TLSEXT_HOSTNAME */, 0, host);
    if (verify && name.len) ossl.set1_host(ssl, host);
    gojo_tls *t = tls_wrap(ssl, (int)fd);
    int64_t r = tls_op(t, TLS_HANDSHAKE, NULL, 0, msg);
    if (r < 0) {
        ossl.ssl_free(ssl);
        pthread_mutex_destroy(&t->mu);
        free(t);
        return r == -2 ? -ECANCELED : 0;
    }
    return (int64_t)(intptr_t)t;
}

// A server context from a PEM certificate chain and key; 0 and *msg on failure.
int64_t gojo_tls___serverCtx(gojo_str cert, gojo_str key, gojo_string *msg) {
    if (!ossl_ready(msg)) return 0;
    char c[4096], k[4096];
    if (cert.len >= (int64_t)sizeof c || key.len >= (int64_t)sizeof k) {
        gojo_fmt_cstr(msg, "path too long");
        return 0;
    }
    if (cert.len) memcpy(c, cert.ptr, (size_t)cert.len);
    c[cert.len] = 0;
    if (key.len) memcpy(k, key.ptr, (size_t)key.len);
    k[key.len] = 0;
    void *ctx = ossl.ctx_new(ossl.server_method());
    if (!ctx) {
        ossl_fail(msg, "cannot create a TLS context");
        return 0;
    }
    ossl_ctx_defaults(ctx);
    if (ossl.ctx_use_chain(ctx, c) != 1) {
        ossl_fail(msg, "cannot load the certificate");
        ossl.ctx_free(ctx);
        return 0;
    }
    if (ossl.ctx_use_key(ctx, k, 1 /* SSL_FILETYPE_PEM */) != 1 || ossl.ctx_check_key(ctx) != 1) {
        ossl_fail(msg, "cannot load the private key (or it doesn't match the certificate)");
        ossl.ctx_free(ctx);
        return 0;
    }
    return (int64_t)(intptr_t)ctx;
}

void gojo_tls___freeCtx(int64_t ctx) {
    if (ctx) ossl.ctx_free((void *)(intptr_t)ctx);
}

// A server connection over an accepted `fd`; the handshake happens on the
// first read or write (or `handshake`).
int64_t gojo_tls___server(int64_t ctx, int64_t fd, gojo_string *msg) {
    void *ssl = ossl.ssl_new((void *)(intptr_t)ctx);
    if (!ssl) {
        ossl_fail(msg, "cannot create a TLS connection");
        return 0;
    }
    ossl.set_fd(ssl, (int)fd);
    ossl.set_accept_state(ssl);
    return (int64_t)(intptr_t)tls_wrap(ssl, (int)fd);
}

// 0 done; -1 failed (*msg); -ECANCELED
int64_t gojo_tls___handshake(int64_t h, gojo_string *msg) {
    int64_t r = tls_op((gojo_tls *)(intptr_t)h, TLS_HANDSHAKE, NULL, 0, msg);
    return r >= 0 ? 0 : r == -2 ? -ECANCELED : -1;
}

// bytes read (0 at the end), -1 failed (*msg), -ECANCELED
int64_t gojo_tls___read(int64_t h, gojo_vec *buf, int64_t max, gojo_string *msg) {
    if (max <= 0) return 0;
    if (max > (1 << 30)) max = 1 << 30;
    gojo_vec_reserve(buf, max, 1);
    int64_t r = tls_op((gojo_tls *)(intptr_t)h, TLS_READ, (uint8_t *)buf->ptr + buf->len, (int)max, msg);
    if (r > 0) buf->len += r;
    return r >= 0 ? r : r == -2 ? -ECANCELED : -1;
}

// 0 all written, -1 failed (*msg), -ECANCELED
int64_t gojo_tls___write(int64_t h, gojo_slice data, gojo_string *msg) {
    gojo_tls *t = (gojo_tls *)(intptr_t)h;
    const uint8_t *p = data.ptr;
    int64_t left = data.len;
    while (left > 0) {
        int n = left > (1 << 30) ? 1 << 30 : (int)left;
        int64_t r = tls_op(t, TLS_WRITE, (void *)p, n, msg);
        if (r < 0) return r == -2 ? -ECANCELED : -1;
        p += r;
        left -= r;
    }
    return 0;
}

// Sends close_notify if the socket takes it now, and frees the connection.
void gojo_tls___close(int64_t h) {
    gojo_tls *t = (gojo_tls *)(intptr_t)h;
    if (!t) return;
    ossl.shutdown(t->ssl);
    ossl.err_clear();
    ossl.ssl_free(t->ssl);
    pthread_mutex_destroy(&t->mu);
    free(t);
}

// ------------------------------------------------------------------ json
//
// `json.encode` and `json.decode` of any type: the compiler generates one
// encoder and one decoder per type (codegen.rs, jenc_fn / jdec_fn), built on
// these. The decoder pulls tokens straight from the text into the typed
// value, with no tree in between.

// a JSON string with the quotes; invalid UTF-8 becomes U+FFFD
void gojo_jenc_str(gojo_string *b, gojo_str s) {
    static const char hex[] = "0123456789abcdef";
    gojo_string_push_byte(b, '"');
    const uint8_t *p = s.ptr;
    int64_t i = 0, run = 0;
    while (i < s.len) {
        uint8_t c = p[i];
        if (c >= 0x20 && c != '"' && c != '\\' && c < 0x80) { i++; continue; }
        if (c >= 0x80) {
            // a valid sequence is kept as is
            int n = (c & 0xE0) == 0xC0 ? 2 : (c & 0xF0) == 0xE0 ? 3 : (c & 0xF8) == 0xF0 ? 4 : 0;
            bool ok = n > 0 && i + n <= s.len;
            uint32_t r = c & (0xFF >> (n + 1));
            for (int k = 1; ok && k < n; k++) {
                if ((p[i + k] & 0xC0) != 0x80) ok = false;
                else r = (r << 6) | (p[i + k] & 0x3F);
            }
            static const uint32_t mins[5] = { 0, 0, 0x80, 0x800, 0x10000 };
            if (ok && r >= mins[n] && r <= 0x10FFFF && (r < 0xD800 || r > 0xDFFF)) { i += n; continue; }
        }
        gojo_str chunk = { p + run, i - run };
        gojo_string_push(b, chunk);
        char esc[8];
        int en = 2;
        esc[0] = '\\';
        switch (c) {
        case '"': esc[1] = '"'; break;
        case '\\': esc[1] = '\\'; break;
        case '\n': esc[1] = 'n'; break;
        case '\r': esc[1] = 'r'; break;
        case '\t': esc[1] = 't'; break;
        default:
            if (c >= 0x80) { memcpy(esc, "\xEF\xBF\xBD", 3); en = 3; }
            else { esc[1] = 'u'; esc[2] = '0'; esc[3] = '0'; esc[4] = hex[c >> 4]; esc[5] = hex[c & 15]; en = 6; }
        }
        gojo_str e = { (const uint8_t *)esc, en };
        gojo_string_push(b, e);
        run = ++i;
    }
    gojo_str chunk = { p + run, i - run };
    gojo_string_push(b, chunk);
    gojo_string_push_byte(b, '"');
}

void gojo_json___quote(gojo_string *b, gojo_str s) { gojo_jenc_str(b, s); }

// JSON has no NaN or infinity: they are written as null
void gojo_jenc_f64(gojo_string *b, double v) {
    if (isnan(v) || isinf(v)) gojo_fmt_cstr(b, "null");
    else gojo_fmt_f64(b, v);
}

// Indents compact JSON text (as the encoder writes it) by `indent` spaces.
void gojo_json___indent(gojo_str s, int64_t indent, gojo_string *out) {
    int64_t depth = 0;
    gojo_String_reserve(out, s.len + s.len / 4);
    for (int64_t i = 0; i < s.len; i++) {
        uint8_t c = s.ptr[i];
        if (c == '"') {
            int64_t j = i + 1;
            while (j < s.len && s.ptr[j] != '"') j += s.ptr[j] == '\\' ? 2 : 1;
            gojo_str t = { s.ptr + i, (j < s.len ? j + 1 : s.len) - i };
            gojo_string_push(out, t);
            i = j;
            continue;
        }
        bool open = c == '[' || c == '{', close = c == ']' || c == '}';
        if (close) {
            depth--;
            // empty: [] and {}
            if (out->len && (out->ptr[out->len - 1] == '[' || out->ptr[out->len - 1] == '{')) { gojo_string_push_byte(out, c); continue; }
            gojo_string_push_byte(out, '\n');
            for (int64_t k = 0; k < depth * indent; k++) gojo_string_push_byte(out, ' ');
        }
        gojo_string_push_byte(out, c);
        if (c == ':') gojo_string_push_byte(out, ' ');
        if (open || c == ',') {
            if (open) depth++;
            if (open && i + 1 < s.len && (s.ptr[i + 1] == ']' || s.ptr[i + 1] == '}')) continue;
            gojo_string_push_byte(out, '\n');
            for (int64_t k = 0; k < depth * indent; k++) gojo_string_push_byte(out, ' ');
        }
    }
}

#define JDEC_MAX_DEPTH 500

void gojo_jdec_init(gojo_jdec *d, gojo_str s) {
    memset(d, 0, sizeof *d);
    d->s = s.ptr;
    d->len = s.len;
    d->errpos = -1;
}

void gojo_jdec_free(gojo_jdec *d) {
    free(d->err.ptr);
    free(d->path.ptr);
    free(d->buf.ptr);
    free(d->kbuf.ptr);
}

static inline void jd_ws(gojo_jdec *d) {
    while (d->pos < d->len) {
        uint8_t c = d->s[d->pos];
        if (c != ' ' && c != '\n' && c != '\r' && c != '\t') break;
        d->pos++;
    }
}

bool gojo_jdec_fail(gojo_jdec *d, const char *msg) {
    if (d->errpos < 0) {
        d->errpos = d->pos;
        gojo_fmt_cstr(&d->err, msg);
    }
    return false;
}

// what the next value is, for messages
static const char *jd_found(gojo_jdec *d) {
    jd_ws(d);
    if (d->pos >= d->len) return "the end of the text";
    switch (d->s[d->pos]) {
    case '"': return "a string";
    case '{': return "an object";
    case '[': return "an array";
    case 't': case 'f': return "a bool";
    case 'n': return "null";
    case '-': case '0': case '1': case '2': case '3': case '4': case '5': case '6': case '7': case '8': case '9': return "a number";
    default: return "an unexpected character";
    }
}

bool gojo_jdec_expected(gojo_jdec *d, const char *what) {
    if (d->errpos >= 0) return false;
    const char *f = jd_found(d);
    gojo_fmt_cstr(&d->err, "expected ");
    gojo_fmt_cstr(&d->err, what);
    gojo_fmt_cstr(&d->err, ", found ");
    gojo_fmt_cstr(&d->err, f);
    d->errpos = d->pos;
    return false;
}

static bool jd_word(gojo_jdec *d, const char *w, int n) {
    if (d->len - d->pos >= n && memcmp(d->s + d->pos, w, (size_t)n) == 0) {
        d->pos += n;
        return true;
    }
    return false;
}

bool gojo_jdec_null(gojo_jdec *d) {
    jd_ws(d);
    return jd_word(d, "null", 4);
}

bool gojo_jdec_bool(gojo_jdec *d, bool *out) {
    jd_ws(d);
    if (jd_word(d, "true", 4)) { *out = true; return true; }
    if (jd_word(d, "false", 5)) { *out = false; return true; }
    return gojo_jdec_expected(d, "true or false");
}

// the extent of a number at pos, checked against JSON's grammar; *frac
// when it has a fraction or an exponent
static int64_t jd_number(gojo_jdec *d, bool *frac) {
    const uint8_t *s = d->s;
    int64_t i = d->pos, n = d->len;
    *frac = false;
    if (i < n && s[i] == '-') i++;
    if (i >= n || s[i] < '0' || s[i] > '9') return -1;
    if (s[i] == '0') {
        i++;
        if (i < n && s[i] >= '0' && s[i] <= '9') {
            if (d->errpos < 0) { d->errpos = i; gojo_fmt_cstr(&d->err, "numbers can't have leading zeros"); }
            return -1;
        }
    } else {
        while (i < n && s[i] >= '0' && s[i] <= '9') i++;
    }
    if (i < n && s[i] == '.') {
        *frac = true;
        i++;
        if (i >= n || s[i] < '0' || s[i] > '9') return -1;
        while (i < n && s[i] >= '0' && s[i] <= '9') i++;
    }
    if (i < n && (s[i] == 'e' || s[i] == 'E')) {
        *frac = true;
        i++;
        if (i < n && (s[i] == '+' || s[i] == '-')) i++;
        if (i >= n || s[i] < '0' || s[i] > '9') return -1;
        while (i < n && s[i] >= '0' && s[i] <= '9') i++;
    }
    return i;
}

static bool jd_range(gojo_jdec *d, int64_t end, const char *ty) {
    if (d->errpos >= 0) return false;
    d->errpos = d->pos;
    gojo_fmt_cstr(&d->err, "the number ");
    gojo_str num = { d->s + d->pos, end - d->pos };
    gojo_string_push(&d->err, num);
    gojo_fmt_cstr(&d->err, " doesn't fit in ");
    gojo_fmt_cstr(&d->err, ty);
    return false;
}

static int64_t jd_int_start(gojo_jdec *d) {
    jd_ws(d);
    bool frac;
    int64_t end = d->pos < d->len ? jd_number(d, &frac) : -1;
    if (end < 0) return -1;
    if (frac) {
        if (d->errpos < 0) {
            d->errpos = d->pos;
            gojo_fmt_cstr(&d->err, "expected an integer, found ");
            gojo_str num = { d->s + d->pos, end - d->pos };
            gojo_string_push(&d->err, num);
        }
        return -2;
    }
    return end;
}

bool gojo_jdec_i64(gojo_jdec *d, int64_t *out, int64_t lo, int64_t hi, const char *ty) {
    int64_t end = jd_int_start(d);
    if (end == -2) return false;
    if (end < 0) return gojo_jdec_expected(d, "an integer");
    bool neg = d->s[d->pos] == '-';
    uint64_t v = 0, limit = neg ? (uint64_t)0 - (uint64_t)lo : (uint64_t)hi;
    for (int64_t i = d->pos + neg; i < end; i++) {
        uint64_t dg = d->s[i] - '0';
        if (v > (limit - dg) / 10) return jd_range(d, end, ty);
        v = v * 10 + dg;
    }
    *out = neg ? (int64_t)((uint64_t)0 - v) : (int64_t)v;
    d->pos = end;
    return true;
}

bool gojo_jdec_u64(gojo_jdec *d, uint64_t *out, uint64_t hi, const char *ty) {
    int64_t end = jd_int_start(d);
    if (end == -2) return false;
    if (end < 0) return gojo_jdec_expected(d, "an integer");
    if (d->s[d->pos] == '-') {
        // -0 is fine
        bool zero = true;
        for (int64_t i = d->pos + 1; i < end; i++) zero = zero && d->s[i] == '0';
        if (!zero) return jd_range(d, end, ty);
        *out = 0;
        d->pos = end;
        return true;
    }
    uint64_t v = 0;
    for (int64_t i = d->pos; i < end; i++) {
        uint64_t dg = d->s[i] - '0';
        if (v > (hi - dg) / 10) return jd_range(d, end, ty);
        v = v * 10 + dg;
    }
    *out = v;
    d->pos = end;
    return true;
}

bool gojo_jdec_f64(gojo_jdec *d, double *out) {
    jd_ws(d);
    bool frac;
    int64_t end = d->pos < d->len ? jd_number(d, &frac) : -1;
    if (end < 0) return gojo_jdec_expected(d, "a number");
    gojo_str num = { d->s + d->pos, end - d->pos };
    if (!gojo_prelude___parseFloat(num, out)) return gojo_jdec_expected(d, "a number");
    if (isinf(*out)) return jd_range(d, end, "f64");
    d->pos = end;
    return true;
}

bool gojo_jdec_f32(gojo_jdec *d, float *out) {
    jd_ws(d);
    bool frac;
    int64_t end = d->pos < d->len ? jd_number(d, &frac) : -1;
    if (end < 0) return gojo_jdec_expected(d, "a number");
    gojo_str num = { d->s + d->pos, end - d->pos };
    double v;
    if (!gojo_prelude___parseFloat(num, &v)) return gojo_jdec_expected(d, "a number");
    if (isinf((float)v)) return jd_range(d, end, "f32");
    *out = (float)v;
    d->pos = end;
    return true;
}

static int jd_hex4(const uint8_t *p) {
    int v = 0;
    for (int k = 0; k < 4; k++) {
        uint8_t c = p[k];
        int x = c >= '0' && c <= '9' ? c - '0' : c >= 'a' && c <= 'f' ? c - 'a' + 10 : c >= 'A' && c <= 'F' ? c - 'A' + 10 : -1;
        if (x < 0) return -1;
        v = v * 16 + x;
    }
    return v;
}

// A string at pos, unescaped: a view of the text when it has no escapes,
// otherwise decoded into *buf (which the view then points into).
static bool jd_string(gojo_jdec *d, gojo_str *view, gojo_string *buf) {
    jd_ws(d);
    if (d->pos >= d->len || d->s[d->pos] != '"') return gojo_jdec_expected(d, "a string");
    const uint8_t *s = d->s;
    int64_t i = d->pos + 1, n = d->len, start = i;
    // fast path: up to the closing quote, without escapes
    while (i < n && s[i] != '"' && s[i] != '\\' && s[i] >= 0x20) i++;
    if (i < n && s[i] == '"') {
        view->ptr = s + start;
        view->len = i - start;
        d->pos = i + 1;
        return true;
    }
    buf->len = 0;
    gojo_str head = { s + start, i - start };
    gojo_string_push(buf, head);
    for (;;) {
        if (i >= n) { d->pos = n; return gojo_jdec_fail(d, "a string has no closing quote"); }
        uint8_t c = s[i];
        if (c == '"') break;
        if (c < 0x20) { d->pos = i; return gojo_jdec_fail(d, "a control character in a string (write it as an escape: \\n, \\u0001)"); }
        if (c != '\\') {
            int64_t j = i;
            while (j < n && s[j] != '"' && s[j] != '\\' && s[j] >= 0x20) j++;
            gojo_str t = { s + i, j - i };
            gojo_string_push(buf, t);
            i = j;
            continue;
        }
        if (i + 1 >= n) { d->pos = n; return gojo_jdec_fail(d, "a string has no closing quote"); }
        uint8_t e = s[i + 1];
        i += 2;
        switch (e) {
        case '"': gojo_string_push_byte(buf, '"'); break;
        case '\\': gojo_string_push_byte(buf, '\\'); break;
        case '/': gojo_string_push_byte(buf, '/'); break;
        case 'b': gojo_string_push_byte(buf, 8); break;
        case 'f': gojo_string_push_byte(buf, 12); break;
        case 'n': gojo_string_push_byte(buf, '\n'); break;
        case 'r': gojo_string_push_byte(buf, '\r'); break;
        case 't': gojo_string_push_byte(buf, '\t'); break;
        case 'u': {
            int u = i + 4 <= n ? jd_hex4(s + i) : -1;
            if (u < 0) { d->pos = i - 2; return gojo_jdec_fail(d, "\\u needs four hex digits"); }
            i += 4;
            int32_t r = u;
            // a surrogate must be the first half of a pair (as json.parse
            // requires): a string can't hold one alone
            bool lone = u >= 0xDC00 && u <= 0xDFFF;
            if (u >= 0xD800 && u <= 0xDBFF) {
                int lo = i + 6 <= n && s[i] == '\\' && s[i + 1] == 'u' ? jd_hex4(s + i + 2) : -1;
                if (lo >= 0xDC00 && lo <= 0xDFFF) {
                    r = 0x10000 + ((u - 0xD800) << 10) + (lo - 0xDC00);
                    i += 6;
                } else {
                    lone = true;
                }
            }
            if (lone) { d->pos = i - 6; return gojo_jdec_fail(d, "lone surrogate in \\u escape"); }
            gojo_String_pushRune(buf, r);
            break;
        }
        default:
            d->pos = i - 2;
            return gojo_jdec_fail(d, "an unknown escape in a string");
        }
    }
    view->ptr = buf->ptr;
    view->len = buf->len;
    d->pos = i + 1;
    return true;
}

// the next character after white space, or -1 at the end
int gojo_jdec_peek(gojo_jdec *d) {
    jd_ws(d);
    return d->pos < d->len ? d->s[d->pos] : -1;
}

// a string as a view, valid until the next string is read
bool gojo_jdec_strview(gojo_jdec *d, gojo_str *out) {
    return jd_string(d, out, &d->buf);
}

bool gojo_jdec_str(gojo_jdec *d, gojo_string *out) {
    gojo_str v;
    if (!jd_string(d, &v, &d->buf)) return false;
    *out = gojo_string_from(v);
    return true;
}

// `[` or `{`
bool gojo_jdec_open(gojo_jdec *d, uint8_t c) {
    jd_ws(d);
    if (d->pos >= d->len || d->s[d->pos] != c) return gojo_jdec_expected(d, c == '[' ? "an array" : "an object");
    if (++d->depth > JDEC_MAX_DEPTH) return gojo_jdec_fail(d, "values are nested more than 500 deep");
    d->pos++;
    return true;
}

// Before element i of an array: 1 when there is one, 0 at the `]`, -1 on
// an error.
int gojo_jdec_item(gojo_jdec *d, int64_t i) {
    jd_ws(d);
    if (d->pos < d->len && d->s[d->pos] == ']' && i == 0) { d->pos++; d->depth--; return 0; }
    if (i > 0) {
        if (d->pos < d->len && d->s[d->pos] == ']') { d->pos++; d->depth--; return 0; }
        if (d->pos >= d->len || d->s[d->pos] != ',') { gojo_jdec_expected(d, "`,` or `]`"); return -1; }
        d->pos++;
        jd_ws(d);
        if (d->pos < d->len && d->s[d->pos] == ']') { gojo_jdec_fail(d, "a `,` before `]`"); return -1; }
    }
    return 1;
}

// Before entry i of an object: 1 with its key in *key (valid until the next
// key) and the `:` read, 0 at the `}`, -1 on an error.
int gojo_jdec_key(gojo_jdec *d, int64_t i, gojo_str *key) {
    jd_ws(d);
    if (d->pos < d->len && d->s[d->pos] == '}') {
        d->pos++;
        d->depth--;
        return 0;
    }
    if (i > 0) {
        if (d->pos >= d->len || d->s[d->pos] != ',') { gojo_jdec_expected(d, "`,` or `}`"); return -1; }
        d->pos++;
        jd_ws(d);
        if (d->pos < d->len && d->s[d->pos] == '}') { gojo_jdec_fail(d, "a `,` before `}`"); return -1; }
    }
    if (d->pos >= d->len || d->s[d->pos] != '"') { gojo_jdec_expected(d, "a string key"); return -1; }
    if (!jd_string(d, key, &d->kbuf)) return -1;
    jd_ws(d);
    if (d->pos >= d->len || d->s[d->pos] != ':') { gojo_jdec_expected(d, "`:`"); return -1; }
    d->pos++;
    return 1;
}

// Skips one value of any kind, checking it.
bool gojo_jdec_skip(gojo_jdec *d) {
    jd_ws(d);
    if (d->pos >= d->len) return gojo_jdec_expected(d, "a value");
    uint8_t c = d->s[d->pos];
    if (c == '"') { gojo_str v; return jd_string(d, &v, &d->buf); }
    if (c == '[') {
        if (!gojo_jdec_open(d, '[')) return false;
        for (int64_t i = 0;; i++) {
            int r = gojo_jdec_item(d, i);
            if (r <= 0) return r == 0;
            if (!gojo_jdec_skip(d)) return false;
        }
    }
    if (c == '{') {
        if (!gojo_jdec_open(d, '{')) return false;
        for (int64_t i = 0;; i++) {
            gojo_str k;
            int r = gojo_jdec_key(d, i, &k);
            if (r <= 0) return r == 0;
            if (!gojo_jdec_skip(d)) return false;
        }
    }
    if (jd_word(d, "true", 4) || jd_word(d, "false", 5) || jd_word(d, "null", 4)) return true;
    bool frac;
    int64_t end = jd_number(d, &frac);
    if (end < 0) return gojo_jdec_expected(d, "a value");
    d->pos = end;
    return true;
}

// Only white space may follow the value.
bool gojo_jdec_end(gojo_jdec *d) {
    jd_ws(d);
    if (d->pos < d->len) return gojo_jdec_fail(d, "more text after the value");
    return true;
}

// An error naming text from the input: a + s + b.
bool gojo_jdec_fail_str(gojo_jdec *d, const char *a, gojo_str s, const char *b) {
    if (d->errpos < 0) {
        d->errpos = d->pos;
        gojo_fmt_cstr(&d->err, a);
        gojo_string_push(&d->err, s);
        gojo_fmt_cstr(&d->err, b);
    }
    return false;
}

// Prepends a step to the path of the value an error is in.
void gojo_jdec_at_key(gojo_jdec *d, const uint8_t *k, int64_t n) {
    gojo_string p = { 0 };
    bool ident = n > 0;
    for (int64_t i = 0; i < n; i++) {
        uint8_t c = k[i];
        ident = ident && ((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || c == '_' || (i > 0 && c >= '0' && c <= '9'));
    }
    gojo_str key = { k, n };
    if (ident) {
        gojo_string_push_byte(&p, '.');
        gojo_string_push(&p, key);
    } else {
        gojo_string_push_byte(&p, '[');
        gojo_jenc_str(&p, key);
        gojo_string_push_byte(&p, ']');
    }
    gojo_string_push(&p, gojo_string_view(&d->path));
    free(d->path.ptr);
    d->path = p;
}

void gojo_jdec_at_index(gojo_jdec *d, int64_t i) {
    gojo_string p = { 0 };
    gojo_string_push_byte(&p, '[');
    gojo_fmt_i64(&p, i);
    gojo_string_push_byte(&p, ']');
    gojo_string_push(&p, gojo_string_view(&d->path));
    free(d->path.ptr);
    d->path = p;
}

// The line and column (in characters), from 1, of byte `pos` of `s`.
void gojo_jdec_linecol(gojo_str s, int64_t pos, int64_t *line, int64_t *col) {
    int64_t l = 1, c = 1;
    for (int64_t i = 0; i < pos && i < s.len; i++) {
        uint8_t b = s.ptr[i];
        if (b == '\n') { l++; c = 1; }
        else if (b < 0x80 || b >= 0xC0) c++;
    }
    *line = l;
    *col = c;
}

// The error message (with the path, when there is one) into *msg; returns
// the byte offset of the error.
int64_t gojo_jdec_result(gojo_jdec *d, gojo_string *msg) {
    if (d->errpos < 0) return -1;
    gojo_string_push(msg, gojo_string_view(&d->err));
    if (d->path.len) {
        gojo_fmt_cstr(msg, " (in ");
        gojo_string_push(msg, gojo_string_view(&d->path));
        gojo_fmt_cstr(msg, ")");
    }
    return d->errpos;
}

// ------------------------------------------------------------------ processes
//
// os.command(...).start(): a child with each standard stream inherited, a
// pipe, /dev/null or (stderr) the same as stdout. Waiting parks the task on
// a pidfd where the kernel has them (Linux 5.3), so cancelling the task stops
// the wait; elsewhere the thread blocks, giving its CPU slot away first.

#if defined(__linux__)
#include <sys/syscall.h>
#endif

// the platform's number for each os.Signal variant, in order
static const int g_signos[] = { SIGHUP, SIGINT, SIGQUIT, SIGKILL, SIGTERM, SIGUSR1, SIGUSR2, SIGWINCH, SIGPIPE, SIGALRM, SIGCHLD, SIGCONT, SIGSTOP, SIGTSTP };
#define NSIGNALS ((int)(sizeof g_signos / sizeof g_signos[0]))

static int signo_of(int64_t code) { return code >= 0 && code < NSIGNALS ? g_signos[code] : -1; }

int64_t gojo_os___getpid(void) { return (int64_t)getpid(); }

int64_t gojo_os___signalCode(int64_t signo) {
    for (int i = 0; i < NSIGNALS; i++) if (g_signos[i] == signo) return i;
    return -1;
}

// modes: 0 inherit, 1 pipe, 2 /dev/null, 3 (stderr) as stdout. `env` holds
// "K=V" strings, used when `setEnv`. Our pipe ends go to fds (3 ints, -1
// for none). Returns an errno.
int64_t gojo_os___spawn(gojo_str prog, gojo_slice args, gojo_slice env, bool setEnv, gojo_str dir, gojo_slice modes, gojo_vec *fds, int64_t *pidOut) {
    if (has_nul(prog) || has_nul(dir)) return EINVAL;
    const gojo_string *as = args.ptr, *es = env.ptr;
    for (int64_t i = 0; i < args.len; i++) if (has_nul(gojo_string_view(&as[i]))) return EINVAL;
    for (int64_t i = 0; i < env.len; i++) if (has_nul(gojo_string_view(&es[i]))) return EINVAL;
    const int64_t *md = modes.ptr;
    char **argv = gojo_alloc(sizeof(char *) * (size_t)(args.len + 2));
    argv[0] = cpath(prog);
    for (int64_t i = 0; i < args.len; i++) argv[i + 1] = cpath(gojo_string_view(&as[i]));
    argv[args.len + 1] = NULL;
    char **envp = environ;
    if (setEnv) {
        envp = gojo_alloc(sizeof(char *) * (size_t)(env.len + 1));
        for (int64_t i = 0; i < env.len; i++) envp[i] = cpath(gojo_string_view(&es[i]));
        envp[env.len] = NULL;
    }
    char *cdir = dir.len ? cpath(dir) : NULL;
    int p[3][2] = { { -1, -1 }, { -1, -1 }, { -1, -1 } };
    int devnull = -1, e = 0;
    posix_spawn_file_actions_t fa;
    posix_spawn_file_actions_init(&fa);
    gojo_blocking_enter();
    pthread_mutex_lock(&g_spawn_lock);
    for (int i = 0; i < 3 && !e; i++) {
        if (md[i] == 1) {
            if (pipe(p[i])) { e = errno; break; }
            fcntl(p[i][0], F_SETFD, FD_CLOEXEC);
            fcntl(p[i][1], F_SETFD, FD_CLOEXEC);
            // the child's end: it reads stdin, writes the others
            posix_spawn_file_actions_adddup2(&fa, i == 0 ? p[i][0] : p[i][1], i);
        } else if (md[i] == 2) {
            if (devnull < 0 && (devnull = open("/dev/null", O_RDWR | O_CLOEXEC)) < 0) { e = errno; break; }
            posix_spawn_file_actions_adddup2(&fa, devnull, i);
        } else if (md[i] == 3 && i == 2) {
            // after stdout is in place
            posix_spawn_file_actions_adddup2(&fa, 1, 2);
        }
    }
    if (!e && cdir) e = posix_spawn_file_actions_addchdir_np(&fa, cdir);
    if (!e) {
        // what this process printed comes before what the child prints
        fflush(stdout);
        fflush(stderr);
    }
    pid_t pid = -1;
    if (!e) e = posix_spawnp(&pid, argv[0], &fa, NULL, argv, envp);
    pthread_mutex_unlock(&g_spawn_lock);
    gojo_ffi_exit();
    posix_spawn_file_actions_destroy(&fa);
    if (devnull >= 0) close(devnull);
    gojo_vec_reserve(fds, 3, sizeof(int64_t));
    int64_t *out = fds->ptr;
    for (int i = 0; i < 3; i++) {
        int mine = i == 0 ? p[i][1] : p[i][0], theirs = i == 0 ? p[i][0] : p[i][1];
        if (theirs >= 0) close(theirs);
        if (e && mine >= 0) { close(mine); mine = -1; }
        out[i] = mine;
    }
    fds->len = 3;
    *pidOut = pid;
    for (int64_t i = 0; i <= args.len; i++) free(argv[i]);
    free(argv);
    if (setEnv) {
        for (int64_t i = 0; i < env.len; i++) free(envp[i]);
        free(envp);
    }
    free(cdir);
    return e;
}

static int64_t exit_status(int st) {
    return WIFEXITED(st) ? WEXITSTATUS(st) : WIFSIGNALED(st) ? 128 + WTERMSIG(st) : 1;
}

// Waits for the child to end: its status, or -errno (-ECANCELED when the
// task was cancelled first; the child keeps running).
int64_t gojo_os___wait(int64_t pid) {
    int st = 0;
#if defined(__linux__) && defined(SYS_pidfd_open)
    if (!gojo_sim_on) {
        int pfd = (int)syscall(SYS_pidfd_open, (pid_t)pid, 0);
        if (pfd >= 0) {
            for (;;) {
                pid_t r = waitpid((pid_t)pid, &st, WNOHANG);
                if (r == (pid_t)pid) { close(pfd); return exit_status(st); }
                if (r < 0 && errno != EINTR) { int e = errno; close(pfd); return -e; }
                if (r == 0 && wait_fd_r(pfd, false, true) == 0) { close(pfd); return -ECANCELED; }
            }
        }
    }
#endif
    if (!gojo_sim_on) {
        // no pidfd (macOS, old kernels): poll, sleeping a little longer each
        // time, so that the wait can be cancelled
        int64_t nap = 500000;
        for (;;) {
            pid_t r = waitpid((pid_t)pid, &st, WNOHANG);
            if (r == (pid_t)pid) return exit_status(st);
            if (r < 0 && errno != EINTR) return -errno;
            if (r == 0 && !gojo_time___sleep(nap)) return -ECANCELED;
            if (nap < 20000000) nap *= 2;
        }
    }
    gojo_blocking_enter();
    pid_t r;
    while ((r = waitpid((pid_t)pid, &st, 0)) < 0 && errno == EINTR) {
    }
    int e = errno;
    gojo_ffi_exit();
    return r < 0 ? -e : exit_status(st);
}

// The status if the child has ended (reaping it), else -1.
int64_t gojo_os___tryWait(int64_t pid) {
    int st = 0;
    pid_t r;
    while ((r = waitpid((pid_t)pid, &st, WNOHANG)) < 0 && errno == EINTR) {
    }
    return r == (pid_t)pid ? exit_status(st) : -1;
}

int64_t gojo_os___kill(int64_t pid, int64_t code) {
    int sig = signo_of(code);
    if (sig < 0) return EINVAL;
    return kill((pid_t)pid, sig) ? errno : 0;
}

// "K=V" for each variable of this process.
void gojo_os___environ(gojo_vec *out) {
    for (char **e = environ; *e; e++) {
        gojo_str s = { (const uint8_t *)*e, (int64_t)strlen(*e) };
        gojo_vec_reserve(out, 1, sizeof(gojo_string));
        ((gojo_string *)out->ptr)[out->len++] = gojo_string_from(s);
    }
}

// ------------------------------------------------------------------ signals
//
// os.notify: the handler writes the signal's number to the write end of a
// pipe per listener (non-blocking, so it never waits), and the listener
// reads it as a File, parked on the poller.

#define SIG_LISTENERS 8
static int g_sig_fds[65][SIG_LISTENERS];  // write fd + 1; 0 for none
static pthread_mutex_t g_sig_lock = PTHREAD_MUTEX_INITIALIZER;
// handlers between reading a write fd and being done with it: a listener
// closes its pipe only when none is, so a handler never writes to a closed
// (or reused) descriptor
static int g_sig_inflight;

static void on_notify(int sig) {
    int saved = errno;
    __atomic_add_fetch(&g_sig_inflight, 1, __ATOMIC_SEQ_CST);
    if (sig > 0 && sig < 65) {
        for (int k = 0; k < SIG_LISTENERS; k++) {
            int f = __atomic_load_n(&g_sig_fds[sig][k], __ATOMIC_SEQ_CST);
            if (f) {
                uint8_t b = (uint8_t)sig;
                ssize_t n = write(f - 1, &b, 1);
                (void)n;
            }
        }
    }
    __atomic_sub_fetch(&g_sig_inflight, 1, __ATOMIC_SEQ_CST);
    errno = saved;
}

// Listens for the signals with these codes: our read end into fds[0], the
// write end (to stop with) into fds[1]. Returns an errno.
int64_t gojo_os___notify(gojo_slice codes, gojo_vec *fds) {
    const int64_t *cs = codes.ptr;
    for (int64_t i = 0; i < codes.len; i++) {
        int s = signo_of(cs[i]);
        if (s < 0 || s >= 65 || s == SIGKILL || s == SIGSTOP) return EINVAL;
    }
    int p[2];
    if (pipe(p)) return errno;
    fcntl(p[0], F_SETFD, FD_CLOEXEC);
    fcntl(p[1], F_SETFD, FD_CLOEXEC);
    fcntl(p[1], F_SETFL, fcntl(p[1], F_GETFL) | O_NONBLOCK);
    pthread_mutex_lock(&g_sig_lock);
    int e = 0;
    for (int64_t i = 0; i < codes.len && !e; i++) {
        int s = signo_of(cs[i]), k = 0;
        while (k < SIG_LISTENERS && g_sig_fds[s][k]) k++;
        if (k == SIG_LISTENERS) { e = EBUSY; break; }
        __atomic_store_n(&g_sig_fds[s][k], p[1] + 1, __ATOMIC_RELEASE);
        struct sigaction sa;
        memset(&sa, 0, sizeof sa);
        sa.sa_handler = on_notify;
        sa.sa_flags = SA_RESTART;
        sigemptyset(&sa.sa_mask);
        sigaction(s, &sa, NULL);
    }
    pthread_mutex_unlock(&g_sig_lock);
    gojo_vec_reserve(fds, 2, sizeof(int64_t));
    ((int64_t *)fds->ptr)[0] = p[0];
    ((int64_t *)fds->ptr)[1] = p[1];
    fds->len = 2;
    return e;
}

// Stops a listener: a signal nobody listens to any more gets its usual
// handling back (Ctrl-C and SIGTERM: shutting the program down).
void gojo_os___unnotify(gojo_slice codes, int64_t wfd) {
    const int64_t *cs = codes.ptr;
    pthread_mutex_lock(&g_sig_lock);
    for (int64_t i = 0; i < codes.len; i++) {
        int s = signo_of(cs[i]);
        if (s < 0 || s >= 65) continue;
        bool any = false;
        for (int k = 0; k < SIG_LISTENERS; k++) {
            if (g_sig_fds[s][k] == (int)wfd + 1) __atomic_store_n(&g_sig_fds[s][k], 0, __ATOMIC_SEQ_CST);
            any = any || g_sig_fds[s][k];
        }
        if (!any) signal(s, s == SIGQUIT ? on_quit : (s == SIGINT || s == SIGTERM) && !gojo_sim_on ? on_shutdown : SIG_DFL);
    }
    pthread_mutex_unlock(&g_sig_lock);
    // a handler that read the fd before it was cleared may still write to it
    while (__atomic_load_n(&g_sig_inflight, __ATOMIC_SEQ_CST)) sched_yield();
    close((int)wfd);
}
