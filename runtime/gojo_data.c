// ------------------------------------------------------------------ yaml and toml
//
// `yaml` and `toml` read text into a tree of nodes (gojo_dtree), and the
// decoders the compiler generates for `json.decode` read their values from
// the tree: the `gojo_jdec_*` functions in gojo_lib.c switch to the `td_*`
// ones here when `d->t` is set. One decoder per type serves all three
// formats. Encoding goes the other way: the encoder the compiler generates
// writes JSON (with floats marked as floats, and NaN and the infinities
// spelled out), which is read into a tree and written as YAML or TOML.
//
// Nodes refer to their children by index. Strings are views of the source
// when they need no unescaping, and live in the tree's arena otherwise.
// Parsers are recursive, with the depth capped at DT_MAX_DEPTH like JSON's.

#define DT_MAX_DEPTH 500

enum { DN_NULL, DN_BOOL, DN_INT, DN_FLOAT, DN_STR, DN_SEQ, DN_MAP };
enum {
    DF_PLAIN = 1,   // a YAML plain scalar: its text also reads as a string
    DF_ARENA = 2,   // DN_STR: the bytes are in the arena, not the source
    DF_U64 = 4,     // DN_INT above INT64_MAX: in v.u
    DF_BIG = 8,     // DN_INT that fits no 64-bit type: only its text
    DF_DATE = 16,   // DN_STR: a TOML date or time
    DF_EMPTY = 32,  // DN_NULL: an empty YAML document (also an empty collection)
};

typedef struct {
    uint8_t kind, flags;
    int32_t n;          // DN_SEQ: items, DN_MAP: pairs
    int64_t src, slen;  // the node's text in the source
    union {
        bool b;
        int64_t i;
        uint64_t u;
        double f;
        struct { int64_t off, len; } s;
        int64_t first;  // DN_SEQ, DN_MAP: in kids (a map has key, value, key...)
    } v;
} dnode;

typedef struct { int32_t node, i; } dframe;

struct gojo_dtree {
    dnode *nodes;
    int64_t nn, cn;
    int32_t *kids;
    int64_t nk, ck;
    int32_t *tmp;       // children of the containers being built
    int64_t nt, ct;
    gojo_string arena;
    const uint8_t *src;
    int64_t len;
    int fmt;
    int32_t root;
    dframe *fr;         // decoding: the containers being read
    int64_t nfr, cfr;
};

// A code point as UTF-8 (without the temporary string of pushRune).
static void dt_rune(gojo_string *o, int32_t r) {
    uint8_t b[4];
    int n;
    if (r < 0x80) { b[0] = (uint8_t)r; n = 1; }
    else if (r < 0x800) { b[0] = (uint8_t)(0xC0 | (r >> 6)); b[1] = (uint8_t)(0x80 | (r & 0x3F)); n = 2; }
    else if (r < 0x10000) { b[0] = (uint8_t)(0xE0 | (r >> 12)); b[1] = (uint8_t)(0x80 | ((r >> 6) & 0x3F)); b[2] = (uint8_t)(0x80 | (r & 0x3F)); n = 3; }
    else { b[0] = (uint8_t)(0xF0 | (r >> 18)); b[1] = (uint8_t)(0x80 | ((r >> 12) & 0x3F)); b[2] = (uint8_t)(0x80 | ((r >> 6) & 0x3F)); b[3] = (uint8_t)(0x80 | (r & 0x3F)); n = 4; }
    gojo_str c = { b, n };
    gojo_string_push(o, c);
}

static void *dt_grow(void *p, int64_t *cap, int64_t need, size_t sz) {
    if (need <= *cap) return p;
    int64_t c = *cap ? *cap : 16;
    while (c < need) c *= 2;
    *cap = c;
    return gojo_realloc(p, (size_t)c * sz);
}

static int32_t dt_node(gojo_dtree *t, int kind, int64_t src) {
    if (t->nn >= INT32_MAX - 1) gojo_panic("the document has too many values", "<runtime>");
    t->nodes = dt_grow(t->nodes, &t->cn, t->nn + 1, sizeof(dnode));
    dnode *n = &t->nodes[t->nn];
    memset(n, 0, sizeof *n);
    n->kind = (uint8_t)kind;
    n->src = src;
    return (int32_t)t->nn++;
}

static void dt_push(gojo_dtree *t, int32_t id) {
    t->tmp = dt_grow(t->tmp, &t->ct, t->nt + 1, sizeof(int32_t));
    t->tmp[t->nt++] = id;
}

// The children pushed since `mark` become those of container `id`.
static void dt_close(gojo_dtree *t, int32_t id, int64_t mark) {
    int64_t k = t->nt - mark;
    t->kids = dt_grow(t->kids, &t->ck, t->nk + k, sizeof(int32_t));
    if (k) memcpy(t->kids + t->nk, t->tmp + mark, (size_t)k * sizeof(int32_t));
    dnode *n = &t->nodes[id];
    n->v.first = t->nk;
    n->n = (int32_t)(n->kind == DN_MAP ? k / 2 : k);
    t->nk += k;
    t->nt = mark;
}

static gojo_str dt_strv(gojo_dtree *t, const dnode *n) {
    gojo_str r = { ((n->flags & DF_ARENA) ? t->arena.ptr : t->src) + n->v.s.off, n->v.s.len };
    if (!r.len) r.ptr = (const uint8_t *)"";
    return r;
}

// The node's text in the source.
static gojo_str dt_text(gojo_dtree *t, const dnode *n) {
    gojo_str r = { t->src + n->src, n->slen };
    if (!r.len) r.ptr = (const uint8_t *)"";
    return r;
}

void gojo_dtree_free(gojo_dtree *t) {
    free(t->nodes);
    free(t->kids);
    free(t->tmp);
    free(t->arena.ptr);
    free(t->fr);
    free(t);
}

static bool dt_fail(gojo_jdec *d, int64_t pos, const char *msg) {
    if (d->errpos < 0) d->pos = pos;
    return gojo_jdec_fail(d, msg);
}

static bool dt_fail_str(gojo_jdec *d, int64_t pos, const char *a, gojo_str s, const char *b) {
    if (d->errpos < 0) d->pos = pos;
    return gojo_jdec_fail_str(d, a, s, b);
}

// a float from text the caller has checked (no length limit, unlike parseFloat)
static double dt_strtod(const uint8_t *p, int64_t n) {
    char small[64];
    char *buf = n < (int64_t)sizeof small ? small : gojo_realloc(NULL, (size_t)n + 1);
    int64_t k = 0;
    for (int64_t i = 0; i < n; i++)
        if (p[i] != '_') buf[k++] = (char)p[i];
    buf[k] = 0;
    double v = strtod(buf, NULL);
    if (buf != small) free(buf);
    return v;
}

// Checks that the text is UTF-8 and, with `ctrl`, has no control
// characters but tab and line breaks: the offset of the first bad byte, or
// -1 (*isctrl says which it is).
static int64_t dt_bad_text(const uint8_t *s, int64_t n, bool ctrl, bool *isctrl) {
    int64_t i = 0;
    *isctrl = false;
    while (i < n) {
        if (i + 8 <= n) {
            uint64_t w;
            memcpy(&w, s + i, 8);
            if (!(w & 0x8080808080808080ULL)) {
                // ASCII: below 0x20 or 0x7F needs a closer look
                uint64_t lo = (w - 0x2020202020202020ULL) & ~w & 0x8080808080808080ULL;
                uint64_t x = w ^ 0x7F7F7F7F7F7F7F7FULL;
                uint64_t del = (x - 0x0101010101010101ULL) & ~x & 0x8080808080808080ULL;
                if (!ctrl || (!lo && !del)) { i += 8; continue; }
            }
        }
        uint8_t c = s[i];
        if (c < 0x80) {
            if (ctrl && ((c < 0x20 && c != '\t' && c != '\n' && c != '\r') || c == 0x7F)) { *isctrl = true; return i; }
            i++;
            continue;
        }
        int k = (c & 0xE0) == 0xC0 ? 2 : (c & 0xF0) == 0xE0 ? 3 : (c & 0xF8) == 0xF0 ? 4 : 0;
        if (!k || i + k > n) return i;
        uint32_t r = c & (0xFF >> (k + 1));
        for (int j = 1; j < k; j++) {
            if ((s[i + j] & 0xC0) != 0x80) return i;
            r = (r << 6) | (s[i + j] & 0x3F);
        }
        static const uint32_t mins[5] = { 0, 0, 0x80, 0x800, 0x10000 };
        if (r < mins[k] || r > 0x10FFFF || (r >= 0xD800 && r <= 0xDFFF)) return i;
        i += k;
    }
    return -1;
}

// ------------------------------------------------------------------ decoding from a tree

static dnode *td_node(gojo_jdec *d) { return d->cur >= 0 ? &d->t->nodes[d->cur] : NULL; }

static const char *td_kind_name(gojo_dtree *t, const dnode *n) {
    if (!n) return "nothing";
    switch (n->kind) {
    case DN_NULL: return "null";
    case DN_BOOL: return "a bool";
    case DN_INT: return "an integer";
    case DN_FLOAT: return "a float";
    case DN_STR: return (n->flags & DF_DATE) ? "a datetime" : "a string";
    case DN_SEQ: return t->fmt == GOJO_FMT_TOML ? "an array" : "a sequence";
    default: return t->fmt == GOJO_FMT_TOML ? "a table" : "a mapping";
    }
}

const char *gojo_td_found(gojo_jdec *d) { return td_kind_name(d->t, td_node(d)); }

// A message with JSON's words for collections in the format's own.
void gojo_td_words(gojo_jdec *d, gojo_string *out, const char *msg) {
    const char *obj = d->t->fmt == GOJO_FMT_TOML ? "a table" : "a mapping";
    const char *arr = d->t->fmt == GOJO_FMT_TOML ? "an array" : "a sequence";
    while (*msg) {
        if (strncmp(msg, "an object", 9) == 0) { gojo_fmt_cstr(out, obj); msg += 9; continue; }
        if (strncmp(msg, "an array", 8) == 0) { gojo_fmt_cstr(out, arr); msg += 8; continue; }
        gojo_string_push_byte(out, (uint8_t)*msg++);
    }
}

static void td_at(gojo_jdec *d, int32_t id) {
    d->cur = id;
    d->pos = d->t->nodes[id].src;
}

int gojo_td_peek(gojo_jdec *d) {
    dnode *n = td_node(d);
    if (!n) return -1;
    switch (n->kind) {
    case DN_NULL: return 'n';
    case DN_BOOL: return (n->flags & DF_PLAIN) ? '"' : 't';
    case DN_INT: case DN_FLOAT: return (n->flags & DF_PLAIN) ? '"' : '0';
    case DN_STR: return '"';
    case DN_SEQ: return '[';
    default: return '{';
    }
}

int gojo_td_kind(gojo_jdec *d) {
    dnode *n = td_node(d);
    if (!n) { gojo_jdec_expected(d, "a value"); return -1; }
    switch (n->kind) {
    case DN_NULL: return 0;
    case DN_BOOL: return 1;
    case DN_INT: return (n->flags & (DF_U64 | DF_BIG)) ? 3 : 2;
    case DN_FLOAT: return 3;
    case DN_STR: return 4;
    case DN_SEQ: return 5;
    default: return 6;
    }
}

bool gojo_td_null(gojo_jdec *d) {
    dnode *n = td_node(d);
    if (!n || n->kind != DN_NULL) return false;
    d->cur = -1;
    return true;
}

bool gojo_td_bool(gojo_jdec *d, bool *out) {
    dnode *n = td_node(d);
    if (!n || n->kind != DN_BOOL) return gojo_jdec_expected(d, "true or false");
    *out = n->v.b;
    d->cur = -1;
    return true;
}

// "the number 300 doesn't fit in u8"
static bool td_range(gojo_jdec *d, dnode *n, const char *ty) {
    if (d->errpos >= 0) return false;
    char what[48];
    snprintf(what, sizeof what, " doesn't fit in %s", ty);
    return gojo_jdec_fail_str(d, "the number ", dt_text(d->t, n), what);
}

static bool td_not_int(gojo_jdec *d, dnode *n) {
    if (n && n->kind == DN_FLOAT) return gojo_jdec_fail_str(d, "expected an integer, found ", dt_text(d->t, n), "");
    return gojo_jdec_expected(d, "an integer");
}

bool gojo_td_i64(gojo_jdec *d, int64_t *out, int64_t lo, int64_t hi, const char *ty) {
    dnode *n = td_node(d);
    if (!n || n->kind != DN_INT) return td_not_int(d, n);
    if ((n->flags & (DF_U64 | DF_BIG)) || n->v.i < lo || n->v.i > hi) return td_range(d, n, ty);
    *out = n->v.i;
    d->cur = -1;
    return true;
}

bool gojo_td_u64(gojo_jdec *d, uint64_t *out, uint64_t hi, const char *ty) {
    dnode *n = td_node(d);
    if (!n || n->kind != DN_INT) return td_not_int(d, n);
    uint64_t v;
    if (n->flags & DF_BIG) return td_range(d, n, ty);
    if (n->flags & DF_U64) v = n->v.u;
    else if (n->v.i < 0) return td_range(d, n, ty);
    else v = (uint64_t)n->v.i;
    if (v > hi) return td_range(d, n, ty);
    *out = v;
    d->cur = -1;
    return true;
}

// any number as a double; `finite`: a float must not overflow to infinity
static bool td_double(gojo_jdec *d, double *out) {
    dnode *n = td_node(d);
    if (!n || (n->kind != DN_INT && n->kind != DN_FLOAT)) return gojo_jdec_expected(d, "a number");
    if (n->kind == DN_FLOAT) *out = n->v.f;
    else if (n->flags & DF_BIG) *out = dt_strtod(d->t->src + n->src, n->slen);
    else if (n->flags & DF_U64) *out = (double)n->v.u;
    else *out = (double)n->v.i;
    d->cur = -1;
    return true;
}

bool gojo_td_f64(gojo_jdec *d, double *out) { return td_double(d, out); }

bool gojo_td_f32(gojo_jdec *d, float *out) {
    dnode *n = td_node(d);
    double v;
    if (!td_double(d, &v)) return false;
    if (isfinite(v) && isinf((float)v)) {
        d->cur = (int32_t)(n - d->t->nodes);
        return td_range(d, n, "f32");
    }
    *out = (float)v;
    return true;
}

bool gojo_td_strview(gojo_jdec *d, gojo_str *out) {
    dnode *n = td_node(d);
    if (n && n->kind == DN_STR) *out = dt_strv(d->t, n);
    // a plain YAML scalar is also its text: `version: 1.10` into a String
    else if (n && (n->flags & DF_PLAIN) && n->kind != DN_NULL) *out = dt_text(d->t, n);
    else return gojo_jdec_expected(d, "a string");
    d->cur = -1;
    return true;
}

bool gojo_td_open(gojo_jdec *d, uint8_t c) {
    dnode *n = td_node(d);
    // an empty document (nothing but comments) is an empty mapping or sequence
    bool empty = n && n->kind == DN_NULL && (n->flags & DF_EMPTY);
    if (!n || (n->kind != (c == '[' ? DN_SEQ : DN_MAP) && !empty)) return gojo_jdec_expected(d, c == '[' ? "an array" : "an object");
    if (++d->depth > DT_MAX_DEPTH) {
        d->nopath = d->errpos < 0;
        return gojo_jdec_fail(d, "values are nested more than 500 deep");
    }
    gojo_dtree *t = d->t;
    t->fr = dt_grow(t->fr, &t->cfr, t->nfr + 1, sizeof(dframe));
    t->fr[t->nfr].node = d->cur;
    t->fr[t->nfr].i = 0;
    t->nfr++;
    d->cur = -1;
    return true;
}

// the next item of the container being read: its index in kids, or -1 at the end
static int64_t td_next(gojo_jdec *d, int step) {
    gojo_dtree *t = d->t;
    dframe *f = &t->fr[t->nfr - 1];
    dnode *m = &t->nodes[f->node];
    if (f->i >= m->n) {
        d->cur = -1;
        d->pos = m->src;
        t->nfr--;
        d->depth--;
        return -1;
    }
    return m->v.first + (int64_t)step * f->i++;
}

int gojo_td_item(gojo_jdec *d) {
    int64_t k = td_next(d, 1);
    if (k < 0) return 0;
    td_at(d, d->t->kids[k]);
    return 1;
}

int gojo_td_key(gojo_jdec *d, gojo_str *key) {
    int64_t k = td_next(d, 2);
    if (k < 0) return 0;
    *key = dt_strv(d->t, &d->t->nodes[d->t->kids[k]]);
    td_at(d, d->t->kids[k + 1]);
    return 1;
}

// A key of the mapping being read matches field `name` (in the loose form
// `norm`) only loosely: 1 when it is the one to use, 0 when the mapping also
// has the exact key (which wins), and -1 (with the error set) when two keys
// match it loosely.
int gojo_td_loose(gojo_jdec *d, const char *name, int64_t n, const char *norm, int64_t nn) {
    gojo_dtree *t = d->t;
    dnode *m = &t->nodes[t->fr[t->nfr - 1].node];
    int32_t first = -1;
    for (int32_t i = 0; i < m->n; i++) {
        dnode *k = &t->nodes[t->kids[m->v.first + 2 * i]];
        gojo_str ks = dt_strv(t, k);
        if (ks.len == n && memcmp(ks.ptr, name, (size_t)n) == 0) return 0;
        if (!gojo_jdec_loose(ks, norm, nn)) continue;
        if (first < 0) { first = i; continue; }
        if (d->errpos < 0) {
            gojo_string msg = { 0 };
            gojo_fmt_cstr(&msg, "the keys \"");
            gojo_string_push(&msg, dt_strv(t, &t->nodes[t->kids[m->v.first + 2 * first]]));
            gojo_fmt_cstr(&msg, "\" and \"");
            gojo_string_push(&msg, ks);
            gojo_fmt_cstr(&msg, "\" both mean the field `");
            gojo_str nm = { (const uint8_t *)name, n };
            gojo_string_push(&msg, nm);
            gojo_fmt_cstr(&msg, "` (keys match fields ignoring case, `-` and `_`)");
            gojo_string_push_byte(&msg, 0);
            d->pos = k->src;
            gojo_jdec_fail(d, (const char *)msg.ptr);
            free(msg.ptr);
        }
        return -1;
    }
    return 1;
}

bool gojo_td_skip(gojo_jdec *d) {
    if (d->cur < 0) return gojo_jdec_expected(d, "a value");
    d->cur = -1;
    return true;
}

// A variant with fields was written without them.
bool gojo_jdec_fail_variant(gojo_jdec *d, const char *variant, const char *type) {
    if (d->errpos >= 0) return false;
    d->errpos = d->pos;
    gojo_fmt_cstr(&d->err, "the variant `");
    gojo_fmt_cstr(&d->err, variant);
    gojo_fmt_cstr(&d->err, "` of `");
    gojo_fmt_cstr(&d->err, type);
    gojo_fmt_cstr(&d->err, "` has fields: write ");
    int fmt = d->t ? d->t->fmt : GOJO_FMT_JSON;
    gojo_fmt_cstr(&d->err, fmt == GOJO_FMT_TOML ? "{ " : fmt == GOJO_FMT_YAML ? "" : "{\"");
    gojo_fmt_cstr(&d->err, variant);
    gojo_fmt_cstr(&d->err, fmt == GOJO_FMT_TOML ? " = { ... } }" : fmt == GOJO_FMT_YAML ? ": { ... }" : "\": {...}}");
    return false;
}

static bool y_scalar(dnode *nd, gojo_str s);

// The text of a map key to read as an integer: in YAML, `0x10` and `+1`
// are integers too, given in decimal; other keys are as written.
gojo_str gojo_jdec_intkey(gojo_jdec *d, gojo_str s, gojo_string *buf) {
    if (!d->t || d->t->fmt != GOJO_FMT_YAML) return s;
    dnode probe = { 0 };
    probe.kind = DN_STR;
    if (!y_scalar(&probe, s) || probe.kind != DN_INT || (probe.flags & DF_BIG)) return s;
    buf->len = 0;
    if (probe.flags & DF_U64) gojo_fmt_u64(buf, probe.v.u);
    else gojo_fmt_i64(buf, probe.v.i);
    return gojo_string_view(buf);
}

// ------------------------------------------------------------------ JSON into a tree
//
// For encoding: the text is what the generated encoder wrote, so it is
// valid; the extensions it writes for YAML and TOML are `NaN`,
// `Infinity` and `-Infinity`.

typedef struct {
    gojo_dtree *t;
    const uint8_t *s;
    int64_t n, i;
} jr;

static bool jr_value(jr *r, int depth);

static int32_t jr_string(jr *r) {
    gojo_dtree *t = r->t;
    int64_t start = ++r->i, i = start;
    while (i < r->n && r->s[i] != '"' && r->s[i] != '\\') i++;
    int32_t id = dt_node(t, DN_STR, start - 1);
    if (i < r->n && r->s[i] == '"') {
        t->nodes[id].v.s.off = start;
        t->nodes[id].v.s.len = i - start;
        r->i = i + 1;
        return id;
    }
    int64_t off = t->arena.len;
    gojo_str head = { r->s + start, i - start };
    gojo_string_push(&t->arena, head);
    while (i < r->n && r->s[i] != '"') {
        uint8_t c = r->s[i];
        if (c != '\\') { gojo_string_push_byte(&t->arena, c); i++; continue; }
        uint8_t e = i + 1 < r->n ? r->s[i + 1] : 0;
        i += 2;
        switch (e) {
        case 'b': gojo_string_push_byte(&t->arena, 8); break;
        case 'f': gojo_string_push_byte(&t->arena, 12); break;
        case 'n': gojo_string_push_byte(&t->arena, '\n'); break;
        case 'r': gojo_string_push_byte(&t->arena, '\r'); break;
        case 't': gojo_string_push_byte(&t->arena, '\t'); break;
        case 'u': {
            int u = i + 4 <= r->n ? jd_hex4(r->s + i) : -1;
            if (u < 0) return -1;
            i += 4;
            int32_t cp = u;
            if (u >= 0xD800 && u <= 0xDBFF && i + 6 <= r->n && r->s[i] == '\\' && r->s[i + 1] == 'u') {
                int lo = jd_hex4(r->s + i + 2);
                if (lo >= 0xDC00 && lo <= 0xDFFF) {
                    cp = 0x10000 + ((u - 0xD800) << 10) + (lo - 0xDC00);
                    i += 6;
                }
            }
            if (cp >= 0xD800 && cp <= 0xDFFF) cp = 0xFFFD;
            dt_rune(&t->arena, cp);
            break;
        }
        default: gojo_string_push_byte(&t->arena, e); break;
        }
    }
    if (i >= r->n) return -1;
    r->i = i + 1;
    t->nodes[id].flags = DF_ARENA;
    t->nodes[id].v.s.off = off;
    t->nodes[id].v.s.len = t->arena.len - off;
    return id;
}

static void jr_ws(jr *r) {
    while (r->i < r->n && (r->s[r->i] == ' ' || r->s[r->i] == '\n' || r->s[r->i] == '\r' || r->s[r->i] == '\t')) r->i++;
}

static bool jr_word(jr *r, const char *w) {
    int64_t k = (int64_t)strlen(w);
    if (r->n - r->i >= k && memcmp(r->s + r->i, w, (size_t)k) == 0) { r->i += k; return true; }
    return false;
}

static bool jr_value(jr *r, int depth) {
    gojo_dtree *t = r->t;
    jr_ws(r);
    if (r->i >= r->n) return false;
    if (depth > DT_MAX_DEPTH) return false;
    uint8_t c = r->s[r->i];
    int64_t at = r->i;
    if (c == '"') {
        int32_t id = jr_string(r);
        if (id < 0) return false;
        dt_push(t, id);
        return true;
    }
    if (c == '[' || c == '{') {
        int32_t id = dt_node(t, c == '[' ? DN_SEQ : DN_MAP, at);
        int64_t mark = t->nt;
        r->i++;
        jr_ws(r);
        uint8_t close = c == '[' ? ']' : '}';
        if (r->i < r->n && r->s[r->i] == close) r->i++;
        else for (;;) {
            if (c == '{') {
                jr_ws(r);
                if (r->i >= r->n || r->s[r->i] != '"') return false;
                int32_t k = jr_string(r);
                if (k < 0) return false;
                dt_push(t, k);
                jr_ws(r);
                if (r->i >= r->n || r->s[r->i] != ':') return false;
                r->i++;
            }
            if (!jr_value(r, depth + 1)) return false;
            jr_ws(r);
            if (r->i >= r->n) return false;
            if (r->s[r->i] == ',') { r->i++; continue; }
            if (r->s[r->i] != close) return false;
            r->i++;
            break;
        }
        dt_close(t, id, mark);
        dt_push(t, id);
        return true;
    }
    if (jr_word(r, "null")) { dt_push(t, dt_node(t, DN_NULL, at)); return true; }
    if (jr_word(r, "true") || jr_word(r, "false")) {
        int32_t id = dt_node(t, DN_BOOL, at);
        t->nodes[id].v.b = c == 't';
        dt_push(t, id);
        return true;
    }
    if (jr_word(r, "NaN") || jr_word(r, "Infinity") || jr_word(r, "-Infinity")) {
        int32_t id = dt_node(t, DN_FLOAT, at);
        t->nodes[id].v.f = c == 'N' ? NAN : c == '-' ? -INFINITY : INFINITY;
        dt_push(t, id);
        return true;
    }
    int64_t i = r->i;
    bool frac = false;
    if (i < r->n && r->s[i] == '-') i++;
    while (i < r->n && ((r->s[i] >= '0' && r->s[i] <= '9') || r->s[i] == '.' || r->s[i] == 'e' || r->s[i] == 'E' || r->s[i] == '+' || r->s[i] == '-')) {
        if (r->s[i] == '.' || r->s[i] == 'e' || r->s[i] == 'E') frac = true;
        i++;
    }
    if (i == r->i) return false;
    int32_t id = dt_node(t, frac ? DN_FLOAT : DN_INT, at);
    dnode *n = &t->nodes[id];
    n->slen = i - at;
    if (frac) {
        n->v.f = dt_strtod(r->s + at, i - at);
    } else {
        bool neg = r->s[at] == '-';
        uint64_t v = 0;
        bool over = false;
        for (int64_t k = at + neg; k < i; k++) {
            uint64_t dg = r->s[k] - '0';
            if (v > (UINT64_MAX - dg) / 10) over = true;
            v = v * 10 + dg;
        }
        if (over || (neg && v > (uint64_t)INT64_MAX + 1)) n->flags = DF_BIG;
        else if (neg) n->v.i = (int64_t)(0 - v);
        else if (v > (uint64_t)INT64_MAX) { n->flags = DF_U64; n->v.u = v; }
        else n->v.i = (int64_t)v;
    }
    r->i = i;
    dt_push(t, id);
    return true;
}

// The tree of JSON text from the encoder, or NULL when it is nested too deep.
static gojo_dtree *jr_tree(gojo_str s, int fmt) {
    gojo_dtree *t = gojo_realloc(NULL, sizeof *t);
    memset(t, 0, sizeof *t);
    t->src = s.ptr;
    t->len = s.len;
    t->fmt = fmt;
    jr r = { t, s.ptr, s.len, 0 };
    if (!jr_value(&r, 0)) {
        gojo_dtree_free(t);
        return NULL;
    }
    t->root = t->tmp[--t->nt];
    return t;
}

// ------------------------------------------------------------------ YAML
//
// YAML 1.2: block and flow collections, plain, quoted and block scalars,
// comments, documents (`---`, `...`), anchors and aliases, merge keys
// (`<<`), and the core schema's tags (`!!str`, `!!int`, ...); other tags are
// ignored. Plain scalars resolve by the core schema: `~`, `null` and
// nothing are null, `true` and `false` are bools (not `yes` or `on`),
// decimal, `0o` and `0x` integers, and floats with `.inf` and `.nan`. A
// key appearing twice in a mapping is an error. Aliases can't make a
// document grow beyond a limit (the "billion laughs").

enum { YC_DOC, YC_SEQ, YC_KEY, YC_MAPV };
enum { YT_NONE, YT_STR, YT_INT, YT_FLOAT, YT_BOOL, YT_NULL, YT_MAP, YT_SEQ, YT_BANG, YT_OTHER };

typedef struct {
    int64_t aoff, alen;   // the anchor's name
    int tag;
    int64_t at;           // where the properties start
    bool anchor, has;
} yprops;

typedef struct { int64_t off, len; int32_t node; } yanchor;

typedef struct {
    gojo_jdec *d;
    gojo_dtree *t;
    const uint8_t *s;
    int64_t n, i, ls;
    int depth;
    yanchor *anc;
    int64_t na, ca;
    int32_t *ah;          // anchors by name: indexes into anc, or -1
    int64_t ahcap;
    bool aliased;
    int64_t merged;
    int32_t *ks;          // the keys of the mapping being closed (a hash set)
    int64_t kscap, ksused;
    // the directives of the document being read
    bool yaml_dir, secondary;   // %YAML seen; %TAG redefines `!!`
    int64_t handles[16][2];     // %TAG handles (`!name!`): offset, length
    int nhandles;
} yp;

static inline bool y_brk(uint8_t c) { return c == '\n' || c == '\r'; }
static inline bool y_blank(uint8_t c) { return c == ' ' || c == '\t'; }
static inline bool y_flowind(uint8_t c) { return c == ',' || c == '[' || c == ']' || c == '{' || c == '}'; }
static inline bool y_wsz(yp *p, int64_t i) { return i >= p->n || y_blank(p->s[i]) || y_brk(p->s[i]); }
static inline uint8_t y_c(yp *p, int64_t k) { return p->i + k < p->n ? p->s[p->i + k] : 0; }
static inline int64_t y_col(yp *p) { return p->i - p->ls; }

static int32_t y_err(yp *p, int64_t pos, const char *msg) {
    dt_fail(p->d, pos, msg);
    return -1;
}

static void y_newline(yp *p) {
    if (p->s[p->i] == '\r' && p->i + 1 < p->n && p->s[p->i + 1] == '\n') p->i++;
    p->i++;
    p->ls = p->i;
}

static void y_blanks(yp *p) {
    while (p->i < p->n && y_blank(p->s[p->i])) p->i++;
}

static void y_to_break(yp *p) {
    while (p->i < p->n && !y_brk(p->s[p->i])) p->i++;
}

// Whether a `#` at i starts a comment: after white space or at a line's start.
static bool y_comment_at(yp *p, int64_t i) {
    return i < p->n && p->s[i] == '#' && (i == 0 || y_blank(p->s[i - 1]) || y_brk(p->s[i - 1]));
}

// Blanks and a comment, up to the end of the line: whether nothing else
// is on it.
static bool y_eol(yp *p) {
    y_blanks(p);
    if (y_comment_at(p, p->i)) y_to_break(p);
    return p->i >= p->n || y_brk(p->s[p->i]);
}

// Whether a tab is among the blanks before p->i on its line: a block
// collection can't start there (tabs don't indent).
static bool y_tabbed(yp *p, int64_t i) {
    for (int64_t k = i - 1; k >= p->ls && y_blank(p->s[k]); k--)
        if (p->s[k] == '\t') return true;
    return false;
}

// `---` or `...` at the start of a line
static bool y_marker(yp *p) {
    if (p->i != p->ls || p->n - p->i < 3) return false;
    const uint8_t *s = p->s + p->i;
    return ((s[0] == '-' && s[1] == '-' && s[2] == '-') || (s[0] == '.' && s[1] == '.' && s[2] == '.')) && y_wsz(p, p->i + 3);
}

// From the end of a line (or the start of one, or content already found):
// skips blank lines and comments to the first character of the next line
// with content. Its indentation (the spaces before it; tabs after them
// only separate), or -1 at the end of the text.
static int64_t y_next_line(yp *p) {
    for (;;) {
        if (p->i >= p->n) return -1;
        if (y_brk(p->s[p->i])) { y_newline(p); continue; }
        int64_t k = p->ls;
        while (k < p->n && p->s[k] == ' ') k++;
        int64_t q = k;
        while (q < p->n && y_blank(p->s[q])) q++;
        if (p->i < q) p->i = q;
        if (p->i >= p->n) return -1;
        uint8_t c = p->s[p->i];
        if (y_brk(c)) continue;
        if (y_comment_at(p, p->i)) { y_to_break(p); continue; }
        return k - p->ls;
    }
}

static uint64_t y_hash(gojo_str s) {
    uint64_t h = 0xcbf29ce484222325ULL;
    for (int64_t i = 0; i < s.len; i++) h = (h ^ s.ptr[i]) * 0x100000001b3ULL;
    return h ^ (h >> 29);
}

static void y_anchor_set(yp *p, int64_t off, int64_t len, int32_t node) {
    gojo_str name = { p->s + off, len };
    if (p->na * 2 >= p->ahcap) {
        int64_t cap = p->ahcap ? p->ahcap * 2 : 64;
        free(p->ah);
        p->ah = gojo_realloc(NULL, (size_t)cap * sizeof(int32_t));
        p->ahcap = cap;
        for (int64_t k = 0; k < cap; k++) p->ah[k] = -1;
        for (int64_t a = 0; a < p->na; a++) {
            gojo_str an = { p->s + p->anc[a].off, p->anc[a].len };
            uint64_t h = y_hash(an) & (uint64_t)(cap - 1);
            while (p->ah[h] >= 0) h = (h + 1) & (uint64_t)(cap - 1);
            p->ah[h] = (int32_t)a;
        }
    }
    uint64_t h = y_hash(name) & (uint64_t)(p->ahcap - 1);
    while (p->ah[h] >= 0) {
        yanchor *a = &p->anc[p->ah[h]];
        if (a->len == len && memcmp(p->s + a->off, name.ptr, (size_t)len) == 0) {
            a->node = node;   // a later anchor with the same name replaces it
            return;
        }
        h = (h + 1) & (uint64_t)(p->ahcap - 1);
    }
    p->anc = dt_grow(p->anc, &p->ca, p->na + 1, sizeof(yanchor));
    p->anc[p->na].off = off;
    p->anc[p->na].len = len;
    p->anc[p->na].node = node;
    p->ah[h] = (int32_t)p->na++;
}

static int32_t y_anchor_get(yp *p, gojo_str name) {
    if (!p->ahcap) return -1;
    uint64_t h = y_hash(name) & (uint64_t)(p->ahcap - 1);
    while (p->ah[h] >= 0) {
        yanchor *a = &p->anc[p->ah[h]];
        if (a->len == name.len && memcmp(p->s + a->off, name.ptr, (size_t)name.len) == 0) return a->node;
        h = (h + 1) & (uint64_t)(p->ahcap - 1);
    }
    return -1;
}

static int y_tag_code(const uint8_t *s, int64_t n) {
    static const char *names[] = { "str", "int", "float", "bool", "null", "map", "seq", "binary", "timestamp" };
    static const int codes[] = { YT_STR, YT_INT, YT_FLOAT, YT_BOOL, YT_NULL, YT_MAP, YT_SEQ, YT_STR, YT_STR };
    if (n == 1) return YT_BANG;
    const uint8_t *name = NULL;
    int64_t len = 0;
    if (n > 2 && s[1] == '!') { name = s + 2; len = n - 2; }
    else if (n > 22 && memcmp(s, "!<tag:yaml.org,2002:", 20) == 0 && s[n - 1] == '>') { name = s + 20; len = n - 21; }
    if (!name) return YT_OTHER;
    for (int k = 0; k < 9; k++)
        if ((int64_t)strlen(names[k]) == len && memcmp(names[k], name, (size_t)len) == 0) return codes[k];
    return YT_OTHER;
}

// A tag's kind, checking that a named handle (`!name!x`) was declared.
static int y_tag(yp *p, int64_t at, int64_t end) {
    const uint8_t *t = p->s + at;
    int64_t n = end - at;
    if (n >= 2 && t[1] == '<') return y_tag_code(t, n);
    if (n >= 2 && t[1] == '!') return p->secondary ? YT_OTHER : y_tag_code(t, n);
    for (int64_t k = 2; k < n; k++) {
        if (t[k] != '!') continue;
        for (int h = 0; h < p->nhandles; h++)
            if (p->handles[h][1] == k + 1 && memcmp(p->s + p->handles[h][0], t, (size_t)k + 1) == 0) return YT_OTHER;
        gojo_str hd = { t, k + 1 };
        dt_fail_str(p->d, at, "the tag handle `", hd, "` isn't declared (with `%TAG` before `---`)");
        return -1;
    }
    return n == 1 ? YT_BANG : YT_OTHER;
}

// Anchors (`&name`) and tags (`!!str`), with blanks between them.
static bool y_props(yp *p, yprops *pr, bool flow) {
    while (p->i < p->n && (p->s[p->i] == '&' || p->s[p->i] == '!')) {
        int64_t at = p->i;
        if (!pr->has) pr->at = at;
        pr->has = true;
        bool anchor = p->s[p->i] == '&';
        p->i++;
        if (!anchor && p->i < p->n && p->s[p->i] == '<') {
            while (p->i < p->n && p->s[p->i] != '>' && !y_brk(p->s[p->i])) p->i++;
            if (p->i >= p->n || p->s[p->i] != '>') return y_err(p, at, "a verbatim tag `!<...>` has no closing `>`") >= 0;
            p->i++;
        } else {
            while (!y_wsz(p, p->i) && !y_flowind(p->s[p->i])) p->i++;
        }
        if (anchor) {
            if (p->i == at + 1) return y_err(p, at, "an anchor needs a name: `&name`") >= 0;
            if (pr->anchor) return y_err(p, at, "a node can have only one anchor") >= 0;
            pr->anchor = true;
            pr->aoff = at + 1;
            pr->alen = p->i - at - 1;
        } else {
            if (pr->tag != YT_NONE) return y_err(p, at, "a node can have only one tag") >= 0;
            pr->tag = y_tag(p, at, p->i);
            if (pr->tag < 0) return false;
        }
        if (!y_wsz(p, p->i) && !(flow && y_flowind(p->s[p->i]))) return y_err(p, p->i, "expected a space after an anchor or a tag") >= 0;
        y_blanks(p);
    }
    return true;
}

static bool y_merge_props(yp *p, yprops *a, const yprops *b) {
    if (!b || !b->has) return true;
    if (!a->has) { *a = *b; return true; }
    if (a->anchor && b->anchor) return y_err(p, b->at, "a node can have only one anchor") >= 0;
    if (a->tag != YT_NONE && b->tag != YT_NONE) return y_err(p, b->at, "a node can have only one tag") >= 0;
    if (b->anchor) { a->anchor = true; a->aoff = b->aoff; a->alen = b->alen; }
    if (b->tag != YT_NONE) a->tag = b->tag;
    return true;
}

static inline bool y_eq(const uint8_t *s, int64_t n, const char *w) {
    return (int64_t)__builtin_strlen(w) == n && memcmp(s, w, (size_t)n) == 0;
}

// An integer's digits: the value, with DF_U64 or DF_BIG when it doesn't fit
// in int64.
static void y_set_int(dnode *nd, uint64_t mag, bool neg, bool over) {
    nd->kind = DN_INT;
    nd->flags &= (uint8_t)~(DF_U64 | DF_BIG | DF_ARENA);
    if (over || (neg && mag > (uint64_t)INT64_MAX + 1)) nd->flags |= DF_BIG;
    else if (neg) nd->v.i = (int64_t)(0 - mag);
    else if (mag > (uint64_t)INT64_MAX) { nd->flags |= DF_U64; nd->v.u = mag; }
    else nd->v.i = (int64_t)mag;
}

// What a plain scalar's text is under the core schema: sets the kind (and
// the value) of `nd`, which is left a string when the text is one. False when
// a float overflows (nd is then unchanged).
static bool y_scalar(dnode *nd, gojo_str s) {
    const uint8_t *c = s.ptr;
    int64_t n = s.len;
    // most scalars are words: only a few first letters can make them anything but a string
    if (n > 0) {
        uint8_t f0 = c[0];
        if (!((f0 >= '0' && f0 <= '9') || f0 == '-' || f0 == '+' || f0 == '.' || f0 == '~' || f0 == 'n' || f0 == 'N' || f0 == 't' || f0 == 'T' || f0 == 'f' || f0 == 'F')) return true;
        if ((f0 == 'n' || f0 == 'N' || f0 == 't' || f0 == 'T') && n != 4) return true;
        if ((f0 == 'f' || f0 == 'F') && n != 5) return true;
    }
    if (n == 0 || (n == 1 && c[0] == '~') || y_eq(c, n, "null") || y_eq(c, n, "Null") || y_eq(c, n, "NULL")) {
        nd->kind = DN_NULL;
        return true;
    }
    if (y_eq(c, n, "true") || y_eq(c, n, "True") || y_eq(c, n, "TRUE") || y_eq(c, n, "false") || y_eq(c, n, "False") || y_eq(c, n, "FALSE")) {
        nd->kind = DN_BOOL;
        nd->v.b = c[0] == 't' || c[0] == 'T';
        return true;
    }
    uint8_t f = c[0];
    if (!((f >= '0' && f <= '9') || f == '-' || f == '+' || f == '.')) return true;
    // 0o17 and 0x1F
    if (n > 2 && c[0] == '0' && (c[1] == 'o' || c[1] == 'x')) {
        int base = c[1] == 'o' ? 8 : 16;
        uint64_t v = 0;
        bool over = false;
        for (int64_t k = 2; k < n; k++) {
            int dg = c[k] >= '0' && c[k] <= '9' ? c[k] - '0' : c[k] >= 'a' && c[k] <= 'f' ? c[k] - 'a' + 10 : c[k] >= 'A' && c[k] <= 'F' ? c[k] - 'A' + 10 : 99;
            if (dg >= base) return true;
            if (v > (UINT64_MAX - (uint64_t)dg) / (uint64_t)base) over = true;
            v = v * (uint64_t)base + (uint64_t)dg;
        }
        y_set_int(nd, v, false, over);
        return true;
    }
    int64_t k = 0;
    bool neg = false;
    if (c[0] == '-' || c[0] == '+') { neg = c[0] == '-'; k = 1; }
    if (k < n && c[k] == '.' && n - k == 4) {
        const uint8_t *w = c + k + 1;
        if (y_eq(w, 3, "inf") || y_eq(w, 3, "Inf") || y_eq(w, 3, "INF")) {
            nd->kind = DN_FLOAT;
            nd->v.f = neg ? -INFINITY : INFINITY;
            return true;
        }
        if (k == 0 && (y_eq(w, 3, "nan") || y_eq(w, 3, "NaN") || y_eq(w, 3, "NAN"))) {
            nd->kind = DN_FLOAT;
            nd->v.f = NAN;
            return true;
        }
    }
    int64_t d0 = k;
    uint64_t v = 0;
    bool over = false;
    while (k < n && c[k] >= '0' && c[k] <= '9') {
        uint64_t dg = c[k] - '0';
        if (v > (UINT64_MAX - dg) / 10) over = true;
        v = v * 10 + dg;
        k++;
    }
    int64_t ndig = k - d0;
    if (k == n) {
        if (ndig) y_set_int(nd, v, neg, over);
        return true;
    }
    int64_t fdig = 0;
    if (c[k] == '.') {
        k++;
        while (k < n && c[k] >= '0' && c[k] <= '9') { k++; fdig++; }
    }
    if (!ndig && !fdig) return true;
    if (k < n && (c[k] == 'e' || c[k] == 'E')) {
        k++;
        if (k < n && (c[k] == '+' || c[k] == '-')) k++;
        int64_t e0 = k;
        while (k < n && c[k] >= '0' && c[k] <= '9') k++;
        if (k == e0) return true;
    }
    if (k != n) return true;
    double x = dt_strtod(c, n);
    if (isinf(x)) return false;
    nd->kind = DN_FLOAT;
    nd->v.f = x;
    return true;
}

static bool y_resolve(yp *p, int32_t id, gojo_str s) {
    if (y_scalar(&p->t->nodes[id], s)) return true;
    dt_fail_str(p->d, p->t->nodes[id].src, "the number ", s, " doesn't fit in f64");
    return false;
}

static void y_set_str_src(dnode *nd, int64_t off, int64_t len) {
    nd->kind = DN_STR;
    nd->flags &= (uint8_t)~(DF_ARENA | DF_U64 | DF_BIG);
    nd->v.s.off = off;
    nd->v.s.len = len;
}

// The text of a scalar node: what it says (a string), or its source.
static gojo_str y_scalar_text(yp *p, const dnode *nd) {
    return nd->kind == DN_STR ? dt_strv(p->t, nd) : dt_text(p->t, nd);
}

// Applies a node's tag and anchor.
static int32_t y_apply(yp *p, const yprops *pr, int32_t id) {
    if (id < 0 || !pr || !pr->has) return id;
    dnode *nd = &p->t->nodes[id];
    int tag = pr->tag;
    bool coll = nd->kind == DN_SEQ || nd->kind == DN_MAP;
    if (tag == YT_MAP || tag == YT_SEQ) {
        if (nd->kind != (tag == YT_MAP ? DN_MAP : DN_SEQ)) return y_err(p, pr->at, tag == YT_MAP ? "`!!map` on a value that isn't a mapping" : "`!!seq` on a value that isn't a sequence");
    } else if (tag == YT_STR || (tag == YT_BANG && (nd->flags & DF_PLAIN))) {
        if (coll) return y_err(p, pr->at, "`!!str` on a collection");
        if (nd->kind != DN_STR) y_set_str_src(nd, nd->src, nd->slen);
        nd->flags &= (uint8_t)~DF_PLAIN;
    } else if (tag == YT_INT || tag == YT_FLOAT || tag == YT_BOOL || tag == YT_NULL) {
        if (coll) return y_err(p, pr->at, "a scalar tag on a collection");
        gojo_str s = y_scalar_text(p, nd);
        dnode save = *nd;
        nd->kind = DN_STR;
        if (!y_resolve(p, id, s)) return -1;
        nd = &p->t->nodes[id];
        int k = nd->kind;
        bool ok = (tag == YT_INT && k == DN_INT) || (tag == YT_FLOAT && (k == DN_INT || k == DN_FLOAT)) || (tag == YT_BOOL && k == DN_BOOL) || (tag == YT_NULL && k == DN_NULL);
        if (!ok) {
            *nd = save;
            static const char *what[] = { "", "", "an integer", "a float", "true or false", "null" };
            gojo_str w = { (const uint8_t *)what[tag], (int64_t)strlen(what[tag]) };
            dt_fail_str(p->d, pr->at, "the tag says ", w, ", but the value isn't one");
            return -1;
        }
        if (tag == YT_FLOAT && k == DN_INT) {
            double x = (nd->flags & DF_BIG) ? dt_strtod(s.ptr, s.len) : (nd->flags & DF_U64) ? (double)nd->v.u : (double)nd->v.i;
            nd->kind = DN_FLOAT;
            nd->flags &= (uint8_t)~(DF_U64 | DF_BIG);
            nd->v.f = x;
        }
    }
    if (pr->anchor) y_anchor_set(p, pr->aoff, pr->alen, id);
    return id;
}

static int32_t y_null_at(yp *p, int64_t at) {
    int32_t id = dt_node(p->t, DN_NULL, at);
    p->t->nodes[id].flags = DF_PLAIN;
    return id;
}

// The empty key of `: value`: an empty string.
static int32_t y_empty_key(yp *p, int64_t at) {
    int32_t id = dt_node(p->t, DN_STR, at);
    p->t->nodes[id].flags = DF_PLAIN;
    y_set_str_src(&p->t->nodes[id], at, 0);
    return id;
}

static int32_t y_alias(yp *p) {
    int64_t at = p->i++;
    while (!y_wsz(p, p->i) && !y_flowind(p->s[p->i])) p->i++;
    gojo_str name = { p->s + at + 1, p->i - at - 1 };
    if (!name.len) return y_err(p, at, "an alias needs a name: `*name`");
    int32_t id = y_anchor_get(p, name);
    if (id < 0) {
        dt_fail_str(p->d, at, "unknown anchor `", name, "` (an alias can only refer to an anchor before it)");
        return -1;
    }
    p->aliased = true;
    return id;
}

// A key's string node.
static int32_t y_key_str(yp *p, int32_t id, int64_t at) {
    if (id < 0) return -1;
    dnode *nd = &p->t->nodes[id];
    if (nd->kind == DN_STR) return id;
    if (nd->kind == DN_SEQ || nd->kind == DN_MAP) return y_err(p, at, "a key must be a scalar (a mapping or a sequence as a key isn't supported)");
    int32_t k = dt_node(p->t, DN_STR, nd->src);
    dnode *kn = &p->t->nodes[k];
    kn->slen = p->t->nodes[id].slen;
    kn->flags = p->t->nodes[id].flags & DF_PLAIN;
    kn->v.s.off = kn->src;
    kn->v.s.len = kn->slen;
    return k;
}

static bool y_is_merge(yp *p, int32_t key) {
    dnode *nd = &p->t->nodes[key];
    if (!(nd->flags & DF_PLAIN) || nd->kind != DN_STR) return false;
    gojo_str s = dt_strv(p->t, nd);
    return s.len == 2 && s.ptr[0] == '<' && s.ptr[1] == '<';
}

static void y_ks_reset(yp *p, int64_t count) {
    int64_t cap = 16;
    while (cap < count * 2) cap *= 2;
    if (cap > p->kscap) {
        free(p->ks);
        p->ks = gojo_realloc(NULL, (size_t)cap * sizeof(int32_t));
        p->kscap = cap;
    }
    for (int64_t k = 0; k < cap; k++) p->ks[k] = -1;
    p->ksused = cap;
}

// Adds a key to the set: false when it is there already.
static bool y_ks_add(yp *p, int32_t key) {
    gojo_str s = dt_strv(p->t, &p->t->nodes[key]);
    uint64_t m = (uint64_t)(p->ksused - 1);
    uint64_t h = y_hash(s) & m;
    while (p->ks[h] >= 0) {
        gojo_str o = dt_strv(p->t, &p->t->nodes[p->ks[h]]);
        if (o.len == s.len && memcmp(o.ptr, s.ptr, (size_t)s.len) == 0) return false;
        h = (h + 1) & m;
    }
    p->ks[h] = key;
    return true;
}

#define Y_MERGE (-1)

// Closes a mapping whose pairs were pushed since `mark`: checks that no key
// is there twice, and replaces each merge (`<<`) by the pairs it brings
// whose keys the mapping doesn't have.
static bool y_close_map(yp *p, int32_t id, int64_t mark) {
    gojo_dtree *t = p->t;
    int64_t np = (t->nt - mark) / 2, extra = 0;
    bool merges = false;
    for (int64_t j = 0; j < np; j++) {
        if (t->tmp[mark + 2 * j] != Y_MERGE) continue;
        merges = true;
        int32_t v = t->tmp[mark + 2 * j + 1];
        dnode *vn = &t->nodes[v];
        if (vn->kind == DN_MAP) extra += vn->n;
        else if (vn->kind == DN_SEQ) {
            for (int32_t k = 0; k < vn->n; k++) {
                dnode *e = &t->nodes[t->kids[vn->v.first + k]];
                if (e->kind != DN_MAP) return y_err(p, e->src, "a merge (`<<`) takes a mapping or a list of mappings") >= 0;
                extra += e->n;
            }
        } else return y_err(p, vn->src, "a merge (`<<`) takes a mapping or a list of mappings") >= 0;
    }
    p->merged += extra;
    if (p->merged > 10000000) return y_err(p, t->nodes[id].src, "merges (`<<`) copy more than 10000000 keys") >= 0;
    if (!merges && np <= 8) {
        // a small mapping: compare its keys with each other
        for (int64_t j = 1; j < np; j++) {
            gojo_str kj = dt_strv(t, &t->nodes[t->tmp[mark + 2 * j]]);
            for (int64_t q = 0; q < j; q++) {
                gojo_str kq = dt_strv(t, &t->nodes[t->tmp[mark + 2 * q]]);
                if (kq.len == kj.len && memcmp(kq.ptr, kj.ptr, (size_t)kj.len) == 0) {
                    dt_fail_str(p->d, t->nodes[t->tmp[mark + 2 * j]].src, "the key \"", kj, "\" appears twice in this mapping");
                    return false;
                }
            }
        }
        dt_close(t, id, mark);
        return true;
    }
    y_ks_reset(p, np + extra);
    for (int64_t j = 0; j < np; j++) {
        int32_t k = t->tmp[mark + 2 * j];
        if (k == Y_MERGE) continue;
        if (!y_ks_add(p, k)) {
            dt_fail_str(p->d, t->nodes[k].src, "the key \"", dt_strv(t, &t->nodes[k]), "\" appears twice in this mapping");
            return false;
        }
    }
    if (merges) {
        int64_t out = t->nt;
        for (int64_t j = 0; j < np; j++) {
            int32_t k = t->tmp[mark + 2 * j], v = t->tmp[mark + 2 * j + 1];
            if (k != Y_MERGE) {
                dt_push(t, k);
                dt_push(t, v);
                continue;
            }
            int32_t nsrc = t->nodes[v].kind == DN_MAP ? 1 : t->nodes[v].n;
            for (int32_t q = 0; q < nsrc; q++) {
                int32_t m = t->nodes[v].kind == DN_MAP ? v : t->kids[t->nodes[v].v.first + q];
                for (int32_t r = 0; r < t->nodes[m].n; r++) {
                    int32_t mk = t->kids[t->nodes[m].v.first + 2 * r], mv = t->kids[t->nodes[m].v.first + 2 * r + 1];
                    if (y_ks_add(p, mk)) {
                        dt_push(t, mk);
                        dt_push(t, mv);
                    }
                }
            }
        }
        int64_t k = t->nt - out;
        memmove(t->tmp + mark, t->tmp + out, (size_t)k * sizeof(int32_t));
        t->nt = mark + k;
    }
    dt_close(t, id, mark);
    return true;
}

static bool y_push_pair(yp *p, int32_t key, int32_t val) {
    if (key < 0 || val < 0) return false;
    dt_push(p->t, y_is_merge(p, key) ? Y_MERGE : key);
    dt_push(p->t, val);
    return true;
}

// The end of a plain scalar's text on this line, from `i`: before a line
// break, ` #`, or a `:` followed by a space (or in flow context by a
// flow indicator); in flow context also before `,[]{}`.
// the bytes that can end a plain scalar, or start the end: 1 in block
// context, 2 in flow context
static const uint8_t y_plain_stop[256] = {
    ['\n'] = 3, ['\r'] = 3, [':'] = 3, [' '] = 3, ['\t'] = 3,
    [','] = 2, ['['] = 2, [']'] = 2, ['{'] = 2, ['}'] = 2,
};

static int64_t y_plain_end(yp *p, int64_t i, bool flow) {
    int64_t end = i;
    uint8_t stop = flow ? 2 : 1;
    while (i < p->n) {
        if (!(y_plain_stop[p->s[i]] & stop)) {
            i++;
            while (i < p->n && !(y_plain_stop[p->s[i]] & stop)) i++;
            end = i;
            continue;
        }
        uint8_t c = p->s[i];
        if (y_brk(c)) break;
        if (c == ':' && (y_wsz(p, i + 1) || (flow && y_flowind(p->s[i + 1])))) break;
        if (flow && y_flowind(c)) break;
        if (y_blank(c)) {
            int64_t j = i;
            while (j < p->n && y_blank(p->s[j])) j++;
            if (j >= p->n || y_brk(p->s[j]) || p->s[j] == '#') break;
            i = j;
            continue;
        }
        i++;
        end = i;
    }
    return end;
}

static bool y_plain_first(yp *p, bool flow) {
    if (p->i >= p->n) return y_err(p, p->i, "expected a value, found the end of the text") >= 0;
    uint8_t c = p->s[p->i];
    if (c == '-' || c == '?' || c == ':') {
        uint8_t nx = y_c(p, 1);
        if (!y_wsz(p, p->i + 1) && !(flow && y_flowind(nx))) return true;
        return y_err(p, p->i, c == ':' ? "a value is missing its key before `:`" : "unexpected indicator") >= 0;
    }
    switch (c) {
    case ',': case '[': case ']': case '{': case '}': case '#': case '&': case '*': case '!': case '|': case '>': case '\'': case '"':
        return y_err(p, p->i, "unexpected character") >= 0;
    case '%':
        return y_err(p, p->i, "a value can't start with `%` (quote it)") >= 0;
    case '@': case '`':
        return y_err(p, p->i, "a value can't start with `@` or a backtick (they are reserved; quote it)") >= 0;
    default:
        return true;
    }
}

// A plain scalar. In block context, when the first line is followed by
// `: `, it is a key: *key is set, its node is a string, and p->i is at the
// `:`. Otherwise it continues on more-indented lines (any in flow
// context), folded into one string, and resolves to its kind.
static int32_t y_plain(yp *p, int64_t indent, bool flow, bool *key) {
    *key = false;
    if (!y_plain_first(p, flow)) return -1;
    gojo_dtree *t = p->t;
    int64_t at = p->i, end = y_plain_end(p, at, flow);
    p->i = end;
    int32_t id = dt_node(t, DN_STR, at);
    t->nodes[id].flags = DF_PLAIN;
    y_set_str_src(&t->nodes[id], at, end - at);
    t->nodes[id].slen = end - at;
    int64_t save = p->i;
    y_blanks(p);
    if (p->i < p->n && p->s[p->i] == ':' && (y_wsz(p, p->i + 1) || (flow && y_flowind(y_c(p, 1))))) {
        if (!flow) *key = true;
        return id;
    }
    p->i = save;
    // more lines
    bool folded = false;
    int64_t aoff = 0;
    for (;;) {
        int64_t i = p->i;
        while (i < p->n && y_blank(p->s[i])) i++;
        if (i >= p->n || !y_brk(p->s[i])) break;
        int64_t empty = 0, ls = p->ls;
        int64_t j = i;
        bool more = false;
        while (j < p->n && y_brk(p->s[j])) {
            if (p->s[j] == '\r' && j + 1 < p->n && p->s[j + 1] == '\n') j++;
            j++;
            ls = j;
            int64_t sp = j;
            while (sp < p->n && p->s[sp] == ' ') sp++;
            int64_t q = sp;
            while (q < p->n && y_blank(p->s[q])) q++;
            if (q >= p->n) break;
            if (y_brk(p->s[q])) { empty++; j = q; continue; }
            if (!flow && sp - ls <= indent) break;
            if (p->s[q] == '#') break;
            if (ls == sp && p->n - sp >= 3 && ((p->s[sp] == '-' && p->s[sp + 1] == '-' && p->s[sp + 2] == '-') || (p->s[sp] == '.' && p->s[sp + 1] == '.' && p->s[sp + 2] == '.')) && (sp + 3 >= p->n || y_blank(p->s[sp + 3]) || y_brk(p->s[sp + 3]))) break;
            if (flow && (y_flowind(p->s[q]) || (p->s[q] == ':' && (q + 1 >= p->n || y_blank(p->s[q + 1]) || y_brk(p->s[q + 1]) || y_flowind(p->s[q + 1]))))) break;
            j = q;
            more = true;
            break;
        }
        if (!more) break;
        // line j continues the scalar
        int64_t e = y_plain_end(p, j, flow);
        if (!folded) {
            aoff = t->arena.len;
            gojo_str first = { p->s + at, end - at };
            gojo_string_push(&t->arena, first);
            folded = true;
        }
        if (empty == 0) gojo_string_push_byte(&t->arena, ' ');
        for (int64_t k = 0; k < empty; k++) gojo_string_push_byte(&t->arena, '\n');
        gojo_str line = { p->s + j, e - j };
        gojo_string_push(&t->arena, line);
        p->ls = ls;
        p->i = e;
        end = e;
        if (!flow) {
            int64_t k = p->i;
            while (k < p->n && y_blank(p->s[k])) k++;
            if (k < p->n && p->s[k] == ':' && y_wsz(p, k + 1)) return y_err(p, k, "a value that spans lines can't contain `: ` (quote it)");
        }
    }
    dnode *nd = &t->nodes[id];
    nd->slen = end - at;
    if (folded) {
        nd->flags |= DF_ARENA;
        nd->v.s.off = aoff;
        nd->v.s.len = t->arena.len - aoff;
        return id;
    }
    gojo_str s = { p->s + at, end - at };
    if (!y_resolve(p, id, s)) return -1;
    return id;
}

static int y_hexval(uint8_t c) {
    return c >= '0' && c <= '9' ? c - '0' : c >= 'a' && c <= 'f' ? c - 'a' + 10 : c >= 'A' && c <= 'F' ? c - 'A' + 10 : -1;
}

static bool y_flow_line(yp *p, int64_t indent);

// A single- or double-quoted scalar; *multi when it spans lines. Its
// lines after the first are indented more than `indent`.
static int32_t y_quoted(yp *p, int64_t indent, bool *multi) {
    gojo_dtree *t = p->t;
    uint8_t q = p->s[p->i];
    int64_t at = p->i++;
    *multi = false;
    // fast path: one line, nothing to unescape
    int64_t i = p->i;
    while (i < p->n && p->s[i] != q && !y_brk(p->s[i]) && !(q == '"' && p->s[i] == '\\')) i++;
    if (i < p->n && p->s[i] == q && !(q == '\'' && i + 1 < p->n && p->s[i + 1] == '\'')) {
        int32_t id = dt_node(t, DN_STR, at);
        y_set_str_src(&t->nodes[id], p->i, i - p->i);
        t->nodes[id].slen = i + 1 - at;
        p->i = i + 1;
        return id;
    }
    int64_t off = t->arena.len, keep = off;
    gojo_str head = { p->s + p->i, i - p->i };
    gojo_string_push(&t->arena, head);
    p->i = i;
    // trailing blanks of a line are dropped: `keep` is the length without them
    for (int64_t k = head.len; k > 0; k--)
        if (!y_blank(head.ptr[k - 1])) { keep = off + k; break; }
    for (;;) {
        if (p->i >= p->n) return y_err(p, at, q == '"' ? "a string has no closing `\"`" : "a string has no closing `'`");
        uint8_t c = p->s[p->i];
        if (c == q) {
            if (q == '\'' && y_c(p, 1) == '\'') {
                gojo_string_push_byte(&t->arena, '\'');
                keep = t->arena.len;
                p->i += 2;
                continue;
            }
            p->i++;
            break;
        }
        if (y_brk(c)) {
            // folding: the line's trailing blanks go, the next line's leading ones too
            t->arena.len = keep;
            *multi = true;
            int64_t empty = 0;
            y_newline(p);
            for (;;) {
                if (!y_flow_line(p, indent)) return -1;
                y_blanks(p);
                if (p->i < p->n && y_brk(p->s[p->i])) { empty++; y_newline(p); continue; }
                break;
            }
            if (empty == 0) gojo_string_push_byte(&t->arena, ' ');
            for (int64_t k = 0; k < empty; k++) gojo_string_push_byte(&t->arena, '\n');
            keep = t->arena.len;
            continue;
        }
        if (q == '"' && c == '\\') {
            int64_t e = p->i;
            uint8_t x = y_c(p, 1);
            p->i += 2;
            int32_t r = -1;
            int hexn = 0;
            switch (x) {
            case '0': r = 0; break;
            case 'a': r = 7; break;
            case 'b': r = 8; break;
            case 't': case '\t': r = 9; break;
            case 'n': r = 10; break;
            case 'v': r = 11; break;
            case 'f': r = 12; break;
            case 'r': r = 13; break;
            case 'e': r = 27; break;
            case ' ': r = ' '; break;
            case '"': r = '"'; break;
            case '/': r = '/'; break;
            case '\\': r = '\\'; break;
            case 'N': r = 0x85; break;
            case '_': r = 0xA0; break;
            case 'L': r = 0x2028; break;
            case 'P': r = 0x2029; break;
            case 'x': hexn = 2; break;
            case 'u': hexn = 4; break;
            case 'U': hexn = 8; break;
            case '\r': case '\n': {
                // an escaped line break joins the lines
                p->i = e + 1;
                y_newline(p);
                int64_t empty = 0;
                for (;;) {
                    if (!y_flow_line(p, indent)) return -1;
                    y_blanks(p);
                    if (p->i < p->n && y_brk(p->s[p->i])) { empty++; y_newline(p); continue; }
                    break;
                }
                for (int64_t k = 0; k < empty; k++) gojo_string_push_byte(&t->arena, '\n');
                keep = t->arena.len;
                *multi = true;
                continue;
            }
            default:
                return y_err(p, e, "an unknown escape in a string");
            }
            if (hexn) {
                uint32_t v = 0;
                for (int k = 0; k < hexn; k++) {
                    int h = p->i < p->n ? y_hexval(p->s[p->i]) : -1;
                    if (h < 0) return y_err(p, e, "an escape needs hex digits");
                    v = v * 16 + (uint32_t)h;
                    p->i++;
                }
                if (v > 0x10FFFF || (v >= 0xD800 && v <= 0xDFFF)) return y_err(p, e, "the escape isn't a Unicode character");
                r = (int32_t)v;
            }
            dt_rune(&t->arena, r);
            keep = t->arena.len;
            continue;
        }
        gojo_string_push_byte(&t->arena, c);
        if (!y_blank(c)) keep = t->arena.len;
        p->i++;
    }
    int32_t id = dt_node(t, DN_STR, at);
    dnode *nd = &t->nodes[id];
    nd->flags = DF_ARENA;
    nd->v.s.off = off;
    nd->v.s.len = t->arena.len - off;
    nd->slen = p->i - at;
    return id;
}

// A literal (`|`) or folded (`>`) block scalar; `indent` is that of the
// node it belongs to. Leaves p->i at the start of the first line after it.
static int32_t y_block_scalar(yp *p, int64_t indent) {
    gojo_dtree *t = p->t;
    int64_t at = p->i;
    bool literal = p->s[p->i++] == '|';
    int chomp = 0, m = 0;
    for (int k = 0; k < 2 && p->i < p->n; k++) {
        uint8_t c = p->s[p->i];
        if ((c == '-' || c == '+') && !chomp) { chomp = c == '-' ? -1 : 1; p->i++; }
        else if (c >= '1' && c <= '9' && !m) { m = c - '0'; p->i++; }
        else break;
    }
    if (!y_eol(p)) return y_err(p, p->i, "expected the end of the line after a block scalar's `|` or `>` (with `-`, `+` or an indentation digit)");
    if (p->i < p->n) y_newline(p);
    int64_t base = indent < 0 ? 0 : indent;
    int64_t ind;
    if (m) ind = base + m;
    else {
        // the indentation of the first line with content
        int64_t j = p->i, maxsp = 0, maxat = -1;
        ind = -1;
        while (j < p->n) {
            int64_t sp = j;
            while (sp < p->n && p->s[sp] == ' ') sp++;
            if (sp < p->n && p->s[sp] == '\t' && sp - j <= indent) return y_err(p, sp, "a tab in a block scalar's indentation (indent with spaces)");
            if (sp < p->n && !y_brk(p->s[sp])) { ind = sp - j; break; }
            if (sp - j > maxsp) { maxsp = sp - j; maxat = j; }
            if (sp >= p->n) break;
            j = sp + (p->s[sp] == '\r' && sp + 1 < p->n && p->s[sp + 1] == '\n' ? 2 : 1);
        }
        int64_t lo = indent + 1;
        if (ind < lo) ind = maxsp > lo ? maxsp : lo;
        else if (maxsp > ind) return y_err(p, maxat, "a blank line at the start of a block scalar has more spaces than its first line");
    }
    int64_t off = t->arena.len, pending = 0;
    bool any = false, prev_spaced = false;
    int64_t last = p->i;
    while (p->i < p->n) {
        int64_t ls = p->i, k = 0;
        if (y_marker(p)) break;
        while (k < ind && p->i < p->n && p->s[p->i] == ' ') { p->i++; k++; }
        if (k < ind) {
            int64_t q = p->i;
            while (q < p->n && y_blank(p->s[q])) q++;
            if (q < p->n && !y_brk(p->s[q])) { p->i = ls; break; }
            if (q > p->i) return y_err(p, p->i, "a tab in a block scalar's indentation (indent with spaces)");
            // an empty line
            p->i = q;
            pending++;
            last = p->i;
            if (p->i < p->n) y_newline(p);
            continue;
        }
        int64_t e = p->i;
        while (e < p->n && !y_brk(p->s[e])) e++;
        gojo_str line = { p->s + p->i, e - p->i };
        if (line.len == 0) {
            pending++;
        } else {
            bool spaced = y_blank(line.ptr[0]);
            if (!any) for (int64_t q = 0; q < pending; q++) gojo_string_push_byte(&t->arena, '\n');
            else if (literal || spaced || prev_spaced) for (int64_t q = 0; q <= pending; q++) gojo_string_push_byte(&t->arena, '\n');
            else if (pending == 0) gojo_string_push_byte(&t->arena, ' ');
            else for (int64_t q = 0; q < pending; q++) gojo_string_push_byte(&t->arena, '\n');
            gojo_string_push(&t->arena, line);
            any = true;
            prev_spaced = spaced;
            pending = 0;
        }
        p->i = e;
        last = e;
        if (p->i < p->n) y_newline(p);
    }
    if (chomp >= 0 && any) gojo_string_push_byte(&t->arena, '\n');
    if (chomp > 0) for (int64_t q = 0; q < pending; q++) gojo_string_push_byte(&t->arena, '\n');
    int32_t id = dt_node(t, DN_STR, at);
    dnode *nd = &t->nodes[id];
    nd->flags = DF_ARENA;
    nd->v.s.off = off;
    nd->v.s.len = t->arena.len - off;
    nd->slen = last - at;
    return id;
}

static int32_t y_flow_node(yp *p, int64_t indent);

// A line inside a multi-line flow value must be indented more than the
// block it is in: checked at the start of each line with content.
static bool y_flow_line(yp *p, int64_t indent) {
    if (y_marker(p)) return y_err(p, p->i, "a document marker inside a value") >= 0;
    int64_t k = p->i;
    while (k < p->n && p->s[k] == ' ') k++;
    int64_t q = k;
    while (q < p->n && y_blank(p->s[q])) q++;
    if (q >= p->n || y_brk(p->s[q]) || p->s[q] == '#') return true;
    if (k - p->ls <= indent) return y_err(p, k, "this line of a value that spans lines must be indented more than the line it starts on") >= 0;
    return true;
}

// White space, line breaks and comments inside a flow collection.
static bool y_flow_ws(yp *p, int64_t open, int64_t indent) {
    for (;;) {
        y_blanks(p);
        if (p->i >= p->n) return y_err(p, open, p->s[open] == '[' ? "a `[` has no closing `]`" : "a `{` has no closing `}`") >= 0;
        uint8_t c = p->s[p->i];
        if (y_comment_at(p, p->i)) { y_to_break(p); continue; }
        if (y_brk(c)) {
            y_newline(p);
            if (!y_flow_line(p, indent)) return false;
            continue;
        }
        return true;
    }
}

// Whether the `:` at p->i makes what came before it a key: it must be
// followed by a space or a flow indicator, unless the key was quoted or a
// collection (JSON style, `{"a":1}`).
static bool y_flow_colon(yp *p, bool adjacent) {
    if (p->i >= p->n || p->s[p->i] != ':') return false;
    return adjacent || y_wsz(p, p->i + 1) || y_flowind(p->s[p->i + 1]);
}

// A value in a flow collection after `key:` (null when there is none).
static int32_t y_flow_value(yp *p, int64_t indent, int64_t open, uint8_t close) {
    int64_t at = p->i;
    if (!y_flow_ws(p, open, indent)) return -1;
    if (p->s[p->i] == ',' || p->s[p->i] == close) return y_null_at(p, at);
    return y_flow_node(p, indent);
}

static int32_t y_flow_coll(yp *p, int64_t indent) {
    gojo_dtree *t = p->t;
    int64_t open = p->i;
    if (++p->depth > DT_MAX_DEPTH) return y_err(p, open, "values are nested more than 500 deep");
    bool seq = p->s[p->i++] == '[';
    uint8_t close = seq ? ']' : '}';
    int32_t id = dt_node(t, seq ? DN_SEQ : DN_MAP, open);
    int64_t mark = t->nt;
    for (;;) {
        if (!y_flow_ws(p, open, indent)) return -1;
        if (p->s[p->i] == close) { p->i++; break; }
        if (p->s[p->i] == ',') return y_err(p, p->i, "a value is missing before `,`");
        int64_t at = p->i;
        bool explicit_key = p->s[p->i] == '?' && (y_wsz(p, p->i + 1) || y_flowind(y_c(p, 1)));
        if (explicit_key) {
            p->i++;
            if (!y_flow_ws(p, open, indent)) return -1;
        }
        int32_t k;
        bool adjacent = false;
        if (explicit_key && (p->s[p->i] == ':' || p->s[p->i] == ',' || p->s[p->i] == close)) {
            k = y_null_at(p, p->i);
        } else if (p->s[p->i] == ':' && (y_wsz(p, p->i + 1) || y_flowind(y_c(p, 1)))) {
            k = y_empty_key(p, p->i);
        } else {
            uint8_t c = p->s[p->i];
            adjacent = c == '"' || c == '\'' || c == '[' || c == '{';
            k = y_flow_node(p, indent);
            if (k < 0) return -1;
            // `&a "x"` and `!!str [..]` still start with a property: what matters is the node
            if (p->i > at && (p->s[p->i - 1] == '"' || p->s[p->i - 1] == '\'' || p->s[p->i - 1] == ']' || p->s[p->i - 1] == '}')) adjacent = true;
        }
        int64_t kline = p->ls;
        if (!y_flow_ws(p, open, indent)) return -1;
        // in a sequence, `key: value` is on one line
        bool pair = y_flow_colon(p, adjacent) && (!seq || explicit_key || p->ls == kline);
        if (seq) {
            if (pair || explicit_key) {
                // `[a: 1]`: a mapping with one pair
                int32_t m = dt_node(t, DN_MAP, at);
                int64_t mk = t->nt;
                int32_t v;
                if (pair) {
                    p->i++;
                    v = y_flow_value(p, indent, open, close);
                } else v = y_null_at(p, p->i);
                if (!y_push_pair(p, y_key_str(p, k, at), v)) return -1;
                if (!y_close_map(p, m, mk)) return -1;
                dt_push(t, m);
            } else dt_push(t, k);
        } else {
            int32_t v;
            if (pair) {
                p->i++;
                v = y_flow_value(p, indent, open, close);
            } else v = y_null_at(p, p->i);
            if (!y_push_pair(p, y_key_str(p, k, at), v)) return -1;
        }
        if (!y_flow_ws(p, open, indent)) return -1;
        if (p->s[p->i] == ',') { p->i++; continue; }
        if (p->s[p->i] == close) { p->i++; break; }
        return y_err(p, p->i, seq ? "expected `,` or `]`" : "expected `,` or `}`");
    }
    if (seq) dt_close(t, id, mark);
    else if (!y_close_map(p, id, mark)) return -1;
    p->depth--;
    return id;
}

static int32_t y_flow_node(yp *p, int64_t indent) {
    yprops pr = { 0 };
    if (!y_props(p, &pr, true)) return -1;
    if (pr.has) {
        int64_t at = p->i;
        // the node may be on the next line
        while (p->i < p->n && (y_blank(p->s[p->i]) || y_brk(p->s[p->i]) || y_comment_at(p, p->i))) {
            if (y_brk(p->s[p->i])) {
                y_newline(p);
                if (!y_flow_line(p, indent)) return -1;
            } else if (p->s[p->i] == '#') y_to_break(p);
            else p->i++;
        }
        uint8_t e = p->i < p->n ? p->s[p->i] : 0;
        if (p->i >= p->n || e == ',' || e == ']' || e == '}' || (e == ':' && (y_wsz(p, p->i + 1) || y_flowind(y_c(p, 1))))) {
            // nothing after the properties: an empty node
            return y_apply(p, &pr, y_null_at(p, at));
        }
    }
    uint8_t c = p->i < p->n ? p->s[p->i] : 0;
    int32_t id;
    if (c == '*') {
        if (pr.has) return y_err(p, pr.at, "an alias can't have an anchor or a tag");
        return y_alias(p);
    }
    if (c == '[' || c == '{') id = y_flow_coll(p, indent);
    else if (c == '"' || c == '\'') {
        bool multi;
        id = y_quoted(p, indent, &multi);
    } else {
        bool key;
        id = y_plain(p, indent, true, &key);
    }
    return y_apply(p, &pr, id);
}

static int32_t y_block_node(yp *p, int64_t indent, int ctx);
static int32_t y_block_content(yp *p, int64_t indent, int64_t col, const yprops *outer, const yprops *inner, bool coll_ok, bool mapv);

static int32_t y_block_seq(yp *p, int64_t col) {
    gojo_dtree *t = p->t;
    if (++p->depth > DT_MAX_DEPTH) return y_err(p, p->i, "values are nested more than 500 deep");
    int32_t id = dt_node(t, DN_SEQ, p->i);
    int64_t mark = t->nt;
    for (;;) {
        p->i++;  // the `-`
        int32_t item = y_block_node(p, col, YC_SEQ);
        if (item < 0) return -1;
        dt_push(t, item);
        int64_t c = y_next_line(p);
        if (c < col || y_marker(p)) break;
        if (c > col) return y_err(p, p->i, "this line is indented more than the sequence's items");
        if (!(p->s[p->i] == '-' && y_wsz(p, p->i + 1))) break;
        if (y_tabbed(p, p->i)) return y_err(p, p->i, "a tab before `- ` (indent with spaces)");
    }
    dt_close(t, id, mark);
    p->depth--;
    return id;
}

// A block mapping at column `col`. `key` is its first key when the caller
// read it (p->i is then just after its `:`), or -1.
static int32_t y_block_map(yp *p, int64_t col, int32_t key) {
    gojo_dtree *t = p->t;
    if (++p->depth > DT_MAX_DEPTH) return y_err(p, p->i, "values are nested more than 500 deep");
    int32_t id = dt_node(t, DN_MAP, key >= 0 ? t->nodes[key].src : p->i);
    int64_t mark = t->nt;
    for (;;) {
        int32_t val;
        if (key < 0) {
            int64_t at = p->i;
            if (p->s[p->i] == '?' && y_wsz(p, p->i + 1)) {
                // an explicit key: `? key` then `: value` on a line of its own
                p->i++;
                key = y_key_str(p, y_block_node(p, col, YC_KEY), at);
                if (key < 0) return -1;
                int64_t c = y_next_line(p);
                if (c == col && !y_marker(p) && p->s[p->i] == ':' && y_wsz(p, p->i + 1)) {
                    p->i++;
                    val = y_block_node(p, col, YC_KEY);
                } else val = y_null_at(p, p->i);
                if (!y_push_pair(p, key, val)) return -1;
                goto next;
            }
            if (y_tabbed(p, p->i)) return y_err(p, p->i, "a tab before a key (indent with spaces)");
            yprops kp = { 0 };
            if (!y_props(p, &kp, false)) return -1;
            uint8_t c = p->i < p->n ? p->s[p->i] : 0;
            int64_t kat = p->i;
            if (c == ':' && y_wsz(p, p->i + 1)) {
                // an empty key: `: value`
                key = y_key_str(p, y_apply(p, &kp, y_empty_key(p, kat)), kat);
                if (key < 0) return -1;
                p->i++;
                val = y_block_node(p, col, YC_MAPV);
                if (!y_push_pair(p, key, val)) return -1;
                goto next;
            }
            if (c == '*') {
                if (kp.has) return y_err(p, kp.at, "an alias can't have an anchor or a tag");
                key = y_alias(p);
            } else if (c == '"' || c == '\'') {
                bool multi;
                key = y_quoted(p, col, &multi);
                if (key >= 0 && multi) return y_err(p, kat, "a key must be on one line");
            } else if (c == '[' || c == '{') {
                return y_err(p, kat, "a key must be a scalar (a mapping or a sequence as a key isn't supported)");
            } else {
                bool isk;
                key = y_plain(p, col, false, &isk);
                if (key >= 0 && !isk) return y_err(p, kat, "expected `key: value` (a line in a mapping needs a `:` after its key)");
            }
            key = y_key_str(p, y_apply(p, &kp, key), kat);
            if (key < 0) return -1;
            y_blanks(p);
            if (!(p->i < p->n && p->s[p->i] == ':' && y_wsz(p, p->i + 1))) return y_err(p, p->i, "expected `:` after the key");
            p->i++;
        }
        val = y_block_node(p, col, YC_MAPV);
        if (!y_push_pair(p, key, val)) return -1;
    next:;
        int64_t c = y_next_line(p);
        if (c < col || y_marker(p)) break;
        if (c > col) return y_err(p, p->i, "this line is indented more than the mapping's keys");
        if (p->s[p->i] == '-' && y_wsz(p, p->i + 1)) return y_err(p, p->i, "a sequence item where a mapping's key was expected");
        key = -1;
    }
    if (!y_close_map(p, id, mark)) return -1;
    p->depth--;
    return id;
}

// A node after `- `, `? `, `: ` or `---`: on the same line, on the lines
// after it, or empty (null). `indent` is the column of the collection it
// is in (-1 at the top).
static int32_t y_block_node(yp *p, int64_t indent, int ctx) {
    y_blanks(p);
    int64_t col = y_col(p);
    yprops pr = { 0 };
    if (!y_props(p, &pr, false)) return -1;
    if (y_eol(p)) {
        int64_t here = p->i;
        int64_t c = y_next_line(p);
        int32_t id;
        if (c < 0 || y_marker(p)) id = y_null_at(p, here);
        else if (c > indent) return y_block_content(p, indent, c, &pr, NULL, true, ctx == YC_MAPV);
        else if (c == indent && ctx == YC_MAPV && p->s[p->i] == '-' && y_wsz(p, p->i + 1)) id = y_block_seq(p, c);
        else id = y_null_at(p, here);
        return y_apply(p, &pr, id);
    }
    // on this line: block collections may start here after `- ` and `? `
    return y_block_content(p, indent, col, NULL, &pr, ctx == YC_SEQ || ctx == YC_KEY, false);
}

// A node starting at column `col` of this line. `outer` are properties
// from an earlier line (they belong to the node); properties on this line
// belong to the key when the node turns out to be a mapping.
static int32_t y_block_content(yp *p, int64_t indent, int64_t col, const yprops *outer, const yprops *inner0, bool coll_ok, bool mapv) {
    yprops op = { 0 }, inner = { 0 };
    if (outer) op = *outer;
    if (inner0 && inner0->has) inner = *inner0;
    else {
        if (!y_props(p, &inner, false)) return -1;
        if (inner.has && y_eol(p)) {
            // properties on a line of their own belong to the node below
            if (!y_merge_props(p, &op, &inner)) return -1;
            int64_t here = p->i;
            int64_t c = y_next_line(p);
            if (c == indent && mapv && !y_marker(p) && p->s[p->i] == '-' && y_wsz(p, p->i + 1)) return y_apply(p, &op, y_block_seq(p, c));
            if (c < 0 || y_marker(p) || c <= indent) return y_apply(p, &op, y_null_at(p, here));
            return y_block_content(p, indent, c, &op, NULL, true, mapv);
        }
    }
    if (p->i >= p->n) return y_apply(p, &op, y_apply(p, &inner, y_null_at(p, p->i)));
    uint8_t c = p->s[p->i];
    bool tab = y_tabbed(p, p->i) || (inner.has && y_tabbed(p, inner.at));
    if ((c == '-' || c == '?' || c == ':') && y_wsz(p, p->i + 1)) {
        if (!coll_ok) return y_err(p, p->i, c == '-' ? "a sequence can't start on the same line as a key (start it on the next line)" : c == '?' ? "unexpected `?` (an explicit key starts its own line)" : "unexpected `:` (a mapping in a value starts on the next line; or quote the value)");
        if (tab) return y_err(p, p->i, "a tab before a block collection (indent with spaces)");
        if (c == ':') {
            // a mapping whose first key is empty: `: value`
            int64_t kat = p->i;
            int32_t key = y_key_str(p, y_apply(p, &inner, y_empty_key(p, kat)), kat);
            if (key < 0) return -1;
            p->i++;
            return y_apply(p, &op, y_block_map(p, col, key));
        }
        if (inner.has) return y_err(p, inner.at, "an anchor or a tag before `- ` or `? ` goes on a line of its own");
        int64_t ccol = y_col(p);
        int32_t id = c == '-' ? y_block_seq(p, ccol) : y_block_map(p, ccol, -1);
        return y_apply(p, &op, id);
    }
    if (c == '|' || c == '>') {
        if (!y_merge_props(p, &op, &inner)) return -1;
        return y_apply(p, &op, y_block_scalar(p, indent));
    }
    int64_t at = p->i;
    int32_t first;
    bool multi = false, coll = false;
    if (c == '*') {
        if (inner.has) return y_err(p, inner.at, "an alias can't have an anchor or a tag");
        first = y_alias(p);
    } else if (c == '[' || c == '{') {
        first = y_flow_coll(p, indent);
        coll = true;
    } else if (c == '"' || c == '\'') {
        first = y_quoted(p, indent, &multi);
    } else {
        bool keyish;
        first = y_plain(p, indent, false, &keyish);
    }
    if (first < 0) return -1;
    int64_t save = p->i;
    y_blanks(p);
    if (p->i < p->n && p->s[p->i] == ':' && y_wsz(p, p->i + 1)) {
        // an implicit key: the node is a mapping
        if (!coll_ok) return y_err(p, p->i, "unexpected `:` (a mapping in a value starts on the next line; or quote the value)");
        if (tab) return y_err(p, at, "a tab before a key (indent with spaces)");
        if (coll) return y_err(p, at, "a key must be a scalar (a mapping or a sequence as a key isn't supported)");
        if (multi) return y_err(p, at, "a key must be on one line");
        int32_t key = y_key_str(p, y_apply(p, &inner, first), at);
        if (key < 0) return -1;
        p->i++;
        return y_apply(p, &op, y_block_map(p, col, key));
    }
    p->i = save;
    if (!y_eol(p)) return y_err(p, p->i, "unexpected text after the value");
    if (!y_merge_props(p, &op, &inner)) return -1;
    return y_apply(p, &op, first);
}

// What a node costs when aliases expand it: one, and one more for each 32
// bytes of a string, so a long string used many times counts.
static int64_t y_cost(const dnode *nd) {
    return 1 + (nd->kind == DN_STR ? nd->v.s.len / 32 : 0);
}

// The expanded size of a node (aliases count each time they are used),
// capped at `limit`; -1 when collections nest deeper than DT_MAX_DEPTH.
// `memo` holds each node's size and `height` the collections nested in it.
static int64_t y_size(gojo_dtree *t, int32_t id, int64_t *memo, int32_t *height, int depth, int64_t limit) {
    if (memo[id]) return depth + height[id] > DT_MAX_DEPTH ? -1 : memo[id];
    dnode *nd = &t->nodes[id];
    int64_t sz = y_cost(nd);
    int32_t h = 0;
    if (nd->kind == DN_SEQ || nd->kind == DN_MAP) {
        if (depth + 1 > DT_MAX_DEPTH) return -1;
        int64_t k = nd->kind == DN_MAP ? 2 * (int64_t)nd->n : nd->n;
        for (int64_t j = 0; j < k; j++) {
            int32_t kid = t->kids[nd->v.first + j];
            int64_t c = y_size(t, kid, memo, height, depth + 1, limit);
            if (c < 0) return -1;
            if (height[kid] > h) h = height[kid];
            sz += c;
            if (sz > limit) sz = limit + 1;
        }
        h++;
    }
    memo[id] = sz;
    height[id] = h;
    return sz;
}

// A directive line: `%YAML 1.2` or `%TAG !h! prefix` (others are ignored).
static bool y_directive(yp *p) {
    int64_t at = p->i, e = at + 1;
    while (!y_wsz(p, e)) e++;
    const uint8_t *name = p->s + at + 1;
    int64_t nl = e - at - 1;
    p->i = e;
    if (y_eq(name, nl, "YAML")) {
        if (p->yaml_dir) return y_err(p, at, "a second %YAML directive for the same document") >= 0;
        p->yaml_dir = true;
        y_blanks(p);
        int64_t d0 = p->i;
        while (p->i < p->n && p->s[p->i] >= '0' && p->s[p->i] <= '9') p->i++;
        bool ok = p->i > d0 && p->i < p->n && p->s[p->i] == '.';
        if (ok) {
            int64_t d1 = ++p->i;
            while (p->i < p->n && p->s[p->i] >= '0' && p->s[p->i] <= '9') p->i++;
            ok = p->i > d1;
        }
        if (!ok || !y_wsz(p, p->i) || !y_eol(p)) return y_err(p, at, "a %YAML directive is `%YAML 1.2`") >= 0;
        return true;
    }
    if (y_eq(name, nl, "TAG")) {
        y_blanks(p);
        int64_t h = p->i;
        if (p->i >= p->n || p->s[p->i] != '!') return y_err(p, at, "a %TAG directive is `%TAG !name! prefix`") >= 0;
        p->i++;
        while (!y_wsz(p, p->i) && p->s[p->i] != '!') p->i++;
        if (p->i < p->n && p->s[p->i] == '!') p->i++;
        int64_t hl = p->i - h;
        if (hl > 1 && p->s[p->i - 1] != '!') return y_err(p, h, "a tag handle is `!`, `!!` or `!name!`") >= 0;
        if (!y_wsz(p, p->i)) return y_err(p, h, "a tag handle is `!`, `!!` or `!name!`") >= 0;
        y_blanks(p);
        int64_t pre = p->i;
        while (!y_wsz(p, p->i)) p->i++;
        if (p->i == pre || !y_eol(p)) return y_err(p, at, "a %TAG directive is `%TAG !name! prefix`") >= 0;
        if (hl == 2) p->secondary = true;
        else if (hl > 2) {
            if (p->nhandles == 16) return y_err(p, at, "more than 16 %TAG directives") >= 0;
            p->handles[p->nhandles][0] = h;
            p->handles[p->nhandles][1] = hl;
            p->nhandles++;
        }
        return true;
    }
    y_to_break(p);
    return true;
}

static bool y_doc_marker(yp *p, char c) {
    return p->i == p->ls && p->n - p->i >= 3 && p->s[p->i] == c && p->s[p->i + 1] == c && p->s[p->i + 2] == c && y_wsz(p, p->i + 3);
}

static bool yaml_parse(gojo_jdec *d, gojo_dtree *t, bool all) {
    yp p = { 0 };
    p.d = d;
    p.t = t;
    p.s = t->src;
    p.n = t->len;
    if (p.n >= 3 && p.s[0] == 0xEF && p.s[1] == 0xBB && p.s[2] == 0xBF) p.i = p.ls = 3;
    bool ok = false;
    int64_t mark = t->nt, ndocs = 0, second = -1;
    for (;;) {
        int64_t c = y_next_line(&p);
        if (c < 0) break;
        bool directives = false;
        p.yaml_dir = p.secondary = false;
        p.nhandles = 0;
        // anchors belong to their document
        p.na = 0;
        if (p.ah) for (int64_t k = 0; k < p.ahcap; k++) p.ah[k] = -1;
        while (c == 0 && p.s[p.i] == '%') {
            directives = true;
            if (!y_directive(&p)) goto done;
            c = y_next_line(&p);
        }
        if (directives && !(c == 0 && y_doc_marker(&p, '-'))) {
            y_err(&p, c < 0 ? p.n : p.i, "a directive (`%...`) must be followed by `---`");
            goto done;
        }
        if (c < 0) break;
        if (c == 0 && y_doc_marker(&p, '.')) {
            p.i += 3;
            if (!y_eol(&p)) { y_err(&p, p.i, "unexpected text after `...`"); goto done; }
            continue;
        }
        if (ndocs == 1) second = p.i;
        int32_t doc;
        if (c == 0 && y_doc_marker(&p, '-')) {
            p.i += 3;
            int64_t at = p.i;
            doc = y_block_node(&p, -1, YC_DOC);
            // `---` with nothing after it
            if (doc >= 0 && t->nodes[doc].kind == DN_NULL && t->nodes[doc].src == at) t->nodes[doc].flags |= DF_EMPTY;
        } else {
            doc = y_block_content(&p, -1, c, NULL, NULL, true, false);
        }
        if (doc < 0) goto done;
        dt_push(t, doc);
        ndocs++;
        c = y_next_line(&p);
        if (c < 0) break;
        if (c == 0 && y_doc_marker(&p, '.')) {
            p.i += 3;
            if (!y_eol(&p)) { y_err(&p, p.i, "unexpected text after `...`"); goto done; }
            continue;
        }
        if (c == 0 && y_doc_marker(&p, '-')) continue;
        y_err(&p, p.i, "expected the end of the document (is this line indented less than the ones before it?)");
        goto done;
    }
    if (all) {
        int32_t root = dt_node(t, DN_SEQ, 0);
        dt_close(t, root, mark);
        t->root = root;
    } else if (ndocs > 1) {
        y_err(&p, second, "the text has more than one document (read them with `yaml.decodeAll` or `yaml.parseAll`)");
        goto done;
    } else if (ndocs == 0) {
        t->root = y_null_at(&p, p.i < p.n ? p.i : p.n);
        t->nodes[t->root].flags |= DF_EMPTY;
    } else {
        t->root = t->tmp[--t->nt];
    }
    if (p.aliased) {
        int64_t *memo = gojo_realloc(NULL, (size_t)t->nn * sizeof(int64_t));
        int32_t *height = gojo_realloc(NULL, (size_t)t->nn * sizeof(int32_t));
        memset(memo, 0, (size_t)t->nn * sizeof(int64_t));
        int64_t base = 0;
        for (int64_t k = 0; k < t->nn; k++) base += y_cost(&t->nodes[k]);
        int64_t limit = 1000000 + 10 * base;
        int64_t sz = y_size(t, t->root, memo, height, all ? -1 : 0, limit);
        free(memo);
        free(height);
        if (sz < 0) { y_err(&p, 0, "values are nested more than 500 deep (through aliases)"); goto done; }
        if (sz > limit) { y_err(&p, 0, "aliases expand the document to too many values"); goto done; }
    }
    ok = true;
done:
    free(p.anc);
    free(p.ah);
    free(p.ks);
    return ok;
}

// ------------------------------------------------------------------ TOML
//
// TOML 1.0, with what 1.1 relaxes (newlines and a trailing comma in inline
// tables, `\e` and `\xHH` escapes, times without seconds). Tables are
// built as the text defines them (headers, dotted keys, arrays of tables)
// and turned into the tree at the end. Dates and times are strings in
// RFC 3339 form (`1979-05-27T07:32:00Z`, `1979-05-27`, `07:32:00`), flagged
// as dates for error messages.

enum { TV_NODE, TV_TAB, TV_AOT };
enum { TF_EXPLICIT = 1, TF_DOTTED = 2, TF_IMPLICIT = 4, TF_INLINE = 8 };

typedef struct { int32_t key; int32_t kind; int32_t ref; } tent;
typedef struct { tent *e; int32_t n, cap; uint8_t flags; bool hashed; int depth; int64_t src; } ttab;
typedef struct { int32_t *tabs; int32_t n, cap; } taot;
typedef struct { int32_t tab, ent; } tslot;

typedef struct {
    gojo_jdec *d;
    gojo_dtree *t;
    const uint8_t *s;
    int64_t n, i;
    ttab *tabs;
    int64_t ntab, ctab;
    taot *aots;
    int64_t naot, caot;
    tslot *h;             // (table, key) -> entry
    int64_t hcap, hn;
    int32_t *kb;          // the parts of the keys being read (a stack)
    int64_t nkb, ckb;
    int depth;            // arrays and inline tables
} tp;

static int32_t t_err(tp *p, int64_t pos, const char *msg) {
    dt_fail(p->d, pos, msg);
    return -1;
}

static inline uint8_t t_c(tp *p, int64_t k) { return p->i + k < p->n ? p->s[p->i + k] : 0; }
static inline bool t_digit(uint8_t c) { return c >= '0' && c <= '9'; }

static void t_ws(tp *p) {
    while (p->i < p->n && (p->s[p->i] == ' ' || p->s[p->i] == '\t')) p->i++;
}

// A comment's characters: anything but control characters (tab is fine).
static bool t_comment(tp *p) {
    while (p->i < p->n && p->s[p->i] != '\n') {
        uint8_t c = p->s[p->i];
        if (c == '\r' && t_c(p, 1) == '\n') break;
        if ((c < 0x20 && c != '\t') || c == 0x7F) return t_err(p, p->i, "a control character in a comment") >= 0;
        p->i++;
    }
    return true;
}

// The end of a line: white space, a comment, and a newline (or the end).
static bool t_eol(tp *p) {
    t_ws(p);
    if (p->i < p->n && p->s[p->i] == '#') {
        p->i++;
        if (!t_comment(p)) return false;
    }
    if (p->i >= p->n) return true;
    if (p->s[p->i] == '\n') { p->i++; return true; }
    if (p->s[p->i] == '\r' && t_c(p, 1) == '\n') { p->i += 2; return true; }
    return t_err(p, p->i, "expected the end of the line") >= 0;
}

// White space, newlines and comments (inside arrays and inline tables).
static bool t_ws_nl(tp *p) {
    for (;;) {
        t_ws(p);
        if (p->i >= p->n) return true;
        uint8_t c = p->s[p->i];
        if (c == '#') {
            p->i++;
            if (!t_comment(p)) return false;
            continue;
        }
        if (c == '\n') { p->i++; continue; }
        if (c == '\r' && t_c(p, 1) == '\n') { p->i += 2; continue; }
        return true;
    }
}

static int32_t t_new_table(tp *p, uint8_t flags, int depth, int64_t src) {
    if (depth > DT_MAX_DEPTH) return t_err(p, src, "tables are nested more than 500 deep");
    p->tabs = dt_grow(p->tabs, &p->ctab, p->ntab + 1, sizeof(ttab));
    ttab *tb = &p->tabs[p->ntab];
    memset(tb, 0, sizeof *tb);
    tb->flags = flags;
    tb->depth = depth;
    tb->src = src;
    return (int32_t)p->ntab++;
}

static uint64_t t_hash(tp *p, int32_t tab, int32_t key) {
    return y_hash(dt_strv(p->t, &p->t->nodes[key])) ^ ((uint64_t)tab * 0x9E3779B97F4A7C15ULL);
}

#define T_SMALL 8

static int32_t t_lookup(tp *p, int32_t tab, int32_t key) {
    gojo_str k = dt_strv(p->t, &p->t->nodes[key]);
    ttab *tb = &p->tabs[tab];
    if (!tb->hashed) {
        for (int32_t e = 0; e < tb->n; e++) {
            gojo_str o = dt_strv(p->t, &p->t->nodes[tb->e[e].key]);
            if (o.len == k.len && memcmp(o.ptr, k.ptr, (size_t)k.len) == 0) return e;
        }
        return -1;
    }
    uint64_t m = (uint64_t)p->hcap - 1, h = t_hash(p, tab, key) & m;
    while (p->h[h].tab >= 0) {
        if (p->h[h].tab == tab) {
            gojo_str o = dt_strv(p->t, &p->t->nodes[p->tabs[tab].e[p->h[h].ent].key]);
            if (o.len == k.len && memcmp(o.ptr, k.ptr, (size_t)k.len) == 0) return p->h[h].ent;
        }
        h = (h + 1) & m;
    }
    return -1;
}

static void t_hash_put(tp *p, int32_t tab, int32_t ent) {
    uint64_t m = (uint64_t)p->hcap - 1, h = t_hash(p, tab, p->tabs[tab].e[ent].key) & m;
    while (p->h[h].tab >= 0) h = (h + 1) & m;
    p->h[h].tab = tab;
    p->h[h].ent = ent;
}

// Room in the hash for `more` entries.
static void t_hash_room(tp *p, int64_t more) {
    if ((p->hn + more) * 2 <= p->hcap) return;
    int64_t cap = p->hcap ? p->hcap * 2 : 64;
    while ((p->hn + more) * 2 > cap) cap *= 2;
    free(p->h);
    p->h = gojo_realloc(NULL, (size_t)cap * sizeof(tslot));
    p->hcap = cap;
    for (int64_t k = 0; k < cap; k++) p->h[k].tab = -1;
    for (int64_t tb = 0; tb < p->ntab; tb++)
        if (p->tabs[tb].hashed)
            for (int32_t e = 0; e < p->tabs[tb].n; e++) t_hash_put(p, (int32_t)tb, e);
}

static int32_t t_add(tp *p, int32_t tab, int32_t key, int kind, int32_t ref) {
    ttab *tb = &p->tabs[tab];
    if (tb->n == tb->cap) {
        tb->cap = tb->cap ? tb->cap * 2 : 4;
        tb->e = gojo_realloc(tb->e, (size_t)tb->cap * sizeof(tent));
    }
    int32_t ent = tb->n++;
    tb->e[ent].key = key;
    tb->e[ent].kind = kind;
    tb->e[ent].ref = ref;
    if (tb->hashed) {
        t_hash_room(p, 1);
        t_hash_put(p, tab, ent);
        p->hn++;
    } else if (tb->n > T_SMALL) {
        // the table grew: its keys go into the hash
        t_hash_room(p, tb->n);
        tb = &p->tabs[tab];
        tb->hashed = true;
        for (int32_t e = 0; e < tb->n; e++) t_hash_put(p, tab, e);
        p->hn += tb->n;
    }
    return ent;
}

static int32_t t_new_aot(tp *p) {
    p->aots = dt_grow(p->aots, &p->caot, p->naot + 1, sizeof(taot));
    memset(&p->aots[p->naot], 0, sizeof(taot));
    return (int32_t)p->naot++;
}

static void t_aot_push(tp *p, int32_t aot, int32_t tab) {
    taot *a = &p->aots[aot];
    if (a->n == a->cap) {
        a->cap = a->cap ? a->cap * 2 : 4;
        a->tabs = gojo_realloc(a->tabs, (size_t)a->cap * sizeof(int32_t));
    }
    a->tabs[a->n++] = tab;
}

static int32_t t_str_node(tp *p, int64_t at, int64_t off, int64_t len, bool arena) {
    int32_t id = dt_node(p->t, DN_STR, at);
    dnode *nd = &p->t->nodes[id];
    nd->flags = arena ? DF_ARENA : 0;
    nd->v.s.off = off;
    nd->v.s.len = len;
    return id;
}

static bool t_ctrl(uint8_t c) { return (c < 0x20 && c != '\t') || c == 0x7F; }

// A basic string ("..." or """...""").
static int32_t t_basic(tp *p, bool multi) {
    gojo_dtree *t = p->t;
    int64_t at = p->i;
    p->i += multi ? 3 : 1;
    if (multi) {
        if (t_c(p, 0) == '\n') p->i++;
        else if (t_c(p, 0) == '\r' && t_c(p, 1) == '\n') p->i += 2;
    } else {
        int64_t j = p->i;
        while (j < p->n && p->s[j] != '"' && p->s[j] != '\\' && !t_ctrl(p->s[j])) j++;
        if (j < p->n && p->s[j] == '"') {
            int32_t id = t_str_node(p, at, p->i, j - p->i, false);
            p->i = j + 1;
            t->nodes[id].slen = p->i - at;
            return id;
        }
    }
    int64_t off = t->arena.len;
    for (;;) {
        if (p->i >= p->n) return t_err(p, at, "a string has no closing quote");
        uint8_t c = p->s[p->i];
        if (c == '"') {
            if (!multi) { p->i++; break; }
            int64_t q = 0;
            while (p->i + q < p->n && p->s[p->i + q] == '"') q++;
            if (q < 3) {
                for (int64_t k = 0; k < q; k++) gojo_string_push_byte(&t->arena, '"');
                p->i += q;
                continue;
            }
            if (q > 5) return t_err(p, p->i, "too many quotes at the end of a string");
            for (int64_t k = 3; k < q; k++) gojo_string_push_byte(&t->arena, '"');
            p->i += q;
            break;
        }
        if (c == '\\') {
            int64_t e = p->i;
            uint8_t x = t_c(p, 1);
            if (multi && (x == ' ' || x == '\t' || x == '\n' || x == '\r')) {
                // a backslash at the end of a line: the line break and the
                // white space after it go
                p->i++;
                t_ws(p);
                if (!(t_c(p, 0) == '\n' || (t_c(p, 0) == '\r' && t_c(p, 1) == '\n'))) return t_err(p, e, "an unknown escape in a string");
                while (p->i < p->n && (p->s[p->i] == ' ' || p->s[p->i] == '\t' || p->s[p->i] == '\n' || (p->s[p->i] == '\r' && t_c(p, 1) == '\n'))) p->i++;
                continue;
            }
            p->i += 2;
            int32_t r;
            int hexn = 0;
            switch (x) {
            case 'b': r = 8; break;
            case 't': r = 9; break;
            case 'n': r = 10; break;
            case 'f': r = 12; break;
            case 'r': r = 13; break;
            case 'e': r = 27; break;
            case '"': r = '"'; break;
            case '\\': r = '\\'; break;
            case 'x': hexn = 2; r = 0; break;
            case 'u': hexn = 4; r = 0; break;
            case 'U': hexn = 8; r = 0; break;
            default: return t_err(p, e, "an unknown escape in a string");
            }
            if (hexn) {
                uint32_t v = 0;
                for (int k = 0; k < hexn; k++) {
                    int h = p->i < p->n ? y_hexval(p->s[p->i]) : -1;
                    if (h < 0) return t_err(p, e, "an escape needs hex digits");
                    v = v * 16 + (uint32_t)h;
                    p->i++;
                }
                if (v > 0x10FFFF || (v >= 0xD800 && v <= 0xDFFF)) return t_err(p, e, "the escape isn't a Unicode character");
                r = (int32_t)v;
            }
            dt_rune(&t->arena, r);
            continue;
        }
        if (c == '\n' || (c == '\r' && t_c(p, 1) == '\n')) {
            if (!multi) return t_err(p, p->i, "a line break in a string (use \"\"\" for a string of several lines)");
            p->i += c == '\r' ? 2 : 1;
            gojo_string_push_byte(&t->arena, '\n');
            continue;
        }
        if (t_ctrl(c)) return t_err(p, p->i, "a control character in a string (write it as an escape: \\u0001)");
        gojo_string_push_byte(&t->arena, c);
        p->i++;
    }
    int32_t id = t_str_node(p, at, off, t->arena.len - off, true);
    t->nodes[id].slen = p->i - at;
    return id;
}

// A literal string ('...' or '''...''').
static int32_t t_literal(tp *p, bool multi) {
    gojo_dtree *t = p->t;
    int64_t at = p->i;
    p->i += multi ? 3 : 1;
    if (multi) {
        if (t_c(p, 0) == '\n') p->i++;
        else if (t_c(p, 0) == '\r' && t_c(p, 1) == '\n') p->i += 2;
    }
    int64_t start = p->i;
    bool crlf = false;
    for (;;) {
        if (p->i >= p->n) return t_err(p, at, "a string has no closing quote");
        uint8_t c = p->s[p->i];
        if (c == '\'') {
            if (!multi) {
                int32_t id = t_str_node(p, at, start, p->i - start, false);
                p->i++;
                t->nodes[id].slen = p->i - at;
                return id;
            }
            int64_t q = 0;
            while (p->i + q < p->n && p->s[p->i + q] == '\'') q++;
            if (q < 3) { p->i += q; continue; }
            if (q > 5) return t_err(p, p->i, "too many quotes at the end of a string");
            int64_t end = p->i + q - 3;
            p->i += q;
            int32_t id;
            if (!crlf) id = t_str_node(p, at, start, end - start, false);
            else {
                // line breaks are \n
                int64_t off = t->arena.len;
                for (int64_t k = start; k < end; k++)
                    if (!(p->s[k] == '\r' && k + 1 < end && p->s[k + 1] == '\n')) gojo_string_push_byte(&t->arena, p->s[k]);
                id = t_str_node(p, at, off, t->arena.len - off, true);
            }
            t->nodes[id].slen = p->i - at;
            return id;
        }
        if (c == '\n' || (c == '\r' && t_c(p, 1) == '\n')) {
            if (!multi) return t_err(p, p->i, "a line break in a string (use ''' for a string of several lines)");
            if (c == '\r') { crlf = true; p->i++; }
            p->i++;
            continue;
        }
        if (t_ctrl(c)) return t_err(p, p->i, "a control character in a string");
        p->i++;
    }
}

// A key's part: bare (A-Z a-z 0-9 _ -) or quoted.
static int32_t t_simple_key(tp *p) {
    int64_t at = p->i;
    uint8_t c = t_c(p, 0);
    if (c == '"' || c == '\'') {
        if (t_c(p, 1) == c && t_c(p, 2) == c) return t_err(p, at, "a key can't be a multi-line string");
        return c == '"' ? t_basic(p, false) : t_literal(p, false);
    }
    while (p->i < p->n) {
        c = p->s[p->i];
        if (!((c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || t_digit(c) || c == '_' || c == '-')) break;
        p->i++;
    }
    if (p->i == at) return t_err(p, at, p->i >= p->n || p->s[p->i] == '\n' || p->s[p->i] == '\r' || p->s[p->i] == '=' ? "expected a key" : "a bare key has only letters, digits, `_` and `-` (quote it)");
    int32_t id = t_str_node(p, at, at, p->i - at, false);
    p->t->nodes[id].slen = p->i - at;
    return id;
}

// A dotted key: its parts pushed onto p->kb (at most DT_MAX_DEPTH); how many.
static int t_key(tp *p) {
    int k = 0;
    for (;;) {
        if (k >= DT_MAX_DEPTH) return t_err(p, p->i, "a key has more than 500 parts");
        int32_t id = t_simple_key(p);
        if (id < 0) return -1;
        p->kb = dt_grow(p->kb, &p->ckb, p->nkb + 1, sizeof(int32_t));
        p->kb[p->nkb++] = id;
        k++;
        t_ws(p);
        if (t_c(p, 0) != '.') return k;
        p->i++;
        t_ws(p);
    }
}

static int64_t t_ndigits(tp *p, int64_t i) {
    int64_t k = i;
    while (k < p->n && t_digit(p->s[k])) k++;
    return k - i;
}

static bool t_delim(tp *p, int64_t i) {
    if (i >= p->n) return true;
    uint8_t c = p->s[i];
    return c == ' ' || c == '\t' || c == '\n' || c == '\r' || c == '#' || c == ',' || c == ']' || c == '}';
}

static int t_num2(const uint8_t *s) { return (s[0] - '0') * 10 + (s[1] - '0'); }

// HH:MM[:SS[.frac]] at i, normalized into the arena (seconds added); the end, or -1.
static int64_t t_time(tp *p, int64_t i, gojo_string *out) {
    const uint8_t *s = p->s;
    if (p->n - i < 5 || !t_digit(s[i]) || !t_digit(s[i + 1]) || s[i + 2] != ':' || !t_digit(s[i + 3]) || !t_digit(s[i + 4])) return -1;
    if (t_num2(s + i) > 23 || t_num2(s + i + 3) > 59) return -1;
    gojo_str hm = { s + i, 5 };
    gojo_string_push(out, hm);
    i += 5;
    if (i < p->n && s[i] == ':') {
        if (p->n - i < 3 || !t_digit(s[i + 1]) || !t_digit(s[i + 2]) || t_num2(s + i + 1) > 60) return -1;
        gojo_str sec = { s + i, 3 };
        gojo_string_push(out, sec);
        i += 3;
        if (i < p->n && s[i] == '.') {
            int64_t k = t_ndigits(p, i + 1);
            if (!k) return -1;
            gojo_str fr = { s + i, k + 1 };
            gojo_string_push(out, fr);
            i += k + 1;
        }
    } else {
        gojo_fmt_cstr(out, ":00");
    }
    return i;
}

// A date, a date-time or a time; -1 (no error set) when it isn't one.
static int32_t t_datetime(tp *p) {
    gojo_dtree *t = p->t;
    const uint8_t *s = p->s;
    int64_t at = p->i, i = at, off = t->arena.len;
    bool date = p->n - i >= 10 && t_ndigits(p, i) == 4 && s[i + 4] == '-';
    if (date) {
        if (!t_digit(s[i + 5]) || !t_digit(s[i + 6]) || s[i + 7] != '-' || !t_digit(s[i + 8]) || !t_digit(s[i + 9])) return t_err(p, at, "a date is YYYY-MM-DD");
        int y = (s[i] - '0') * 1000 + (s[i + 1] - '0') * 100 + t_num2(s + i + 2), mo = t_num2(s + i + 5), dd = t_num2(s + i + 8);
        static const int mdays[12] = { 31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31 };
        bool leap = (y % 4 == 0 && y % 100 != 0) || y % 400 == 0;
        if (mo < 1 || mo > 12 || dd < 1 || dd > mdays[mo - 1] || (mo == 2 && dd == 29 && !leap)) return t_err(p, at, "the date doesn't exist");
        gojo_str ds = { s + i, 10 };
        gojo_string_push(&t->arena, ds);
        i += 10;
        if (i < p->n && (s[i] == 'T' || s[i] == 't' || (s[i] == ' ' && i + 1 < p->n && t_digit(s[i + 1])))) {
            gojo_string_push_byte(&t->arena, 'T');
            i = t_time(p, i + 1, &t->arena);
            if (i < 0) return t_err(p, at, "a time is HH:MM:SS (with an optional fraction)");
            if (i < p->n && (s[i] == 'Z' || s[i] == 'z')) {
                gojo_string_push_byte(&t->arena, 'Z');
                i++;
            } else if (i < p->n && (s[i] == '+' || s[i] == '-')) {
                if (p->n - i < 6 || !t_digit(s[i + 1]) || !t_digit(s[i + 2]) || s[i + 3] != ':' || !t_digit(s[i + 4]) || !t_digit(s[i + 5]) || t_num2(s + i + 1) > 23 || t_num2(s + i + 4) > 59) return t_err(p, at, "a time offset is Z or +HH:MM");
                gojo_str o = { s + i, 6 };
                gojo_string_push(&t->arena, o);
                i += 6;
            }
        }
    } else {
        i = t_time(p, i, &t->arena);
        if (i < 0) {
            t->arena.len = off;
            return t_err(p, at, "a time is HH:MM:SS (with an optional fraction)");
        }
    }
    if (!t_delim(p, i)) return t_err(p, at, "unexpected text after a date or time");
    p->i = i;
    int32_t id = t_str_node(p, at, off, t->arena.len - off, true);
    t->nodes[id].flags |= DF_DATE;
    t->nodes[id].slen = i - at;
    return id;
}

// Digits with `_` only between digits, in `base`, from i; the end, or -1.
static int64_t t_digits_us(tp *p, int64_t i, int base) {
    int64_t st = i;
    bool prev_digit = false;
    while (i < p->n) {
        uint8_t c = p->s[i];
        int v = y_hexval(c);
        if (v >= 0 && v < base && !(base <= 10 && !t_digit(c))) { prev_digit = true; i++; continue; }
        if (c == '_' && prev_digit && i + 1 < p->n && y_hexval(p->s[i + 1]) >= 0 && y_hexval(p->s[i + 1]) < base) { prev_digit = false; i++; continue; }
        break;
    }
    return i > st && prev_digit ? i : -1;
}

static int32_t t_number(tp *p) {
    gojo_dtree *t = p->t;
    const uint8_t *s = p->s;
    int64_t at = p->i, i = at;
    if ((p->n - i >= 5 && t_ndigits(p, i) == 4 && s[i + 4] == '-') || (p->n - i >= 3 && t_ndigits(p, i) == 2 && s[i + 2] == ':')) return t_datetime(p);
    bool neg = false, sign = false;
    if (s[i] == '+' || s[i] == '-') { neg = s[i] == '-'; sign = true; i++; }
    if (p->n - i >= 3 && (memcmp(s + i, "inf", 3) == 0 || memcmp(s + i, "nan", 3) == 0)) {
        if (!t_delim(p, i + 3)) return t_err(p, at, "invalid value");
        int32_t id = dt_node(t, DN_FLOAT, at);
        t->nodes[id].v.f = s[i] == 'n' ? NAN : neg ? -INFINITY : INFINITY;
        t->nodes[id].slen = i + 3 - at;
        p->i = i + 3;
        return id;
    }
    if (i < p->n && s[i] == '0' && i + 1 < p->n && (s[i + 1] == 'x' || s[i + 1] == 'o' || s[i + 1] == 'b')) {
        if (sign) return t_err(p, at, "a hex, octal or binary integer has no sign");
        int base = s[i + 1] == 'x' ? 16 : s[i + 1] == 'o' ? 8 : 2;
        int64_t e = t_digits_us(p, i + 2, base);
        if (e < 0 || !t_delim(p, e)) return t_err(p, at, "invalid integer");
        uint64_t v = 0;
        for (int64_t k = i + 2; k < e; k++) {
            if (s[k] == '_') continue;
            uint64_t dg = (uint64_t)y_hexval(s[k]);
            if (v > ((uint64_t)INT64_MAX - dg) / (uint64_t)base) return t_err(p, at, "the integer doesn't fit in 64 bits");
            v = v * (uint64_t)base + dg;
        }
        int32_t id = dt_node(t, DN_INT, at);
        t->nodes[id].v.i = (int64_t)v;
        t->nodes[id].slen = e - at;
        p->i = e;
        return id;
    }
    if (i >= p->n || !t_digit(s[i])) return t_err(p, at, "expected a value");
    int64_t e = t_digits_us(p, i, 10);
    if (e < 0) return t_err(p, at, "invalid number");
    if (s[i] == '0' && e - i > 1) return t_err(p, at, "a number can't have leading zeros");
    bool frac = false;
    if (e < p->n && s[e] == '.') {
        int64_t f = t_digits_us(p, e + 1, 10);
        if (f < 0) return t_err(p, at, "a `.` in a number needs digits on both sides");
        e = f;
        frac = true;
    }
    if (e < p->n && (s[e] == 'e' || s[e] == 'E')) {
        int64_t x = e + 1;
        if (x < p->n && (s[x] == '+' || s[x] == '-')) x++;
        int64_t f = t_digits_us(p, x, 10);
        if (f < 0) return t_err(p, at, "an exponent needs digits");
        e = f;
        frac = true;
    }
    if (!t_delim(p, e)) return t_err(p, at, "invalid number");
    int32_t id;
    if (frac) {
        double v = dt_strtod(s + at, e - at);
        if (isinf(v)) {
            gojo_str num = { s + at, e - at };
            dt_fail_str(p->d, at, "the number ", num, " doesn't fit in f64");
            return -1;
        }
        id = dt_node(t, DN_FLOAT, at);
        t->nodes[id].v.f = v;
    } else {
        uint64_t v = 0;
        for (int64_t k = i; k < e; k++) {
            if (s[k] == '_') continue;
            uint64_t dg = s[k] - '0';
            if (v > ((uint64_t)INT64_MAX + 1 - dg) / 10) return t_err(p, at, "the integer doesn't fit in 64 bits");
            v = v * 10 + dg;
        }
        if (!neg && v > (uint64_t)INT64_MAX) return t_err(p, at, "the integer doesn't fit in 64 bits");
        id = dt_node(t, DN_INT, at);
        t->nodes[id].v.i = neg ? (int64_t)(0 - v) : (int64_t)v;
    }
    t->nodes[id].slen = e - at;
    p->i = e;
    return id;
}

static int32_t t_value(tp *p);
static int32_t t_build(tp *p, int32_t tab);
static bool t_keyval(tp *p, int32_t tab, uint8_t dotted_flags);

static int32_t t_array(tp *p) {
    gojo_dtree *t = p->t;
    int64_t at = p->i++;
    if (++p->depth > DT_MAX_DEPTH) return t_err(p, at, "values are nested more than 500 deep");
    int32_t id = dt_node(t, DN_SEQ, at);
    int64_t mark = t->nt;
    for (;;) {
        if (!t_ws_nl(p)) return -1;
        if (p->i >= p->n) return t_err(p, at, "a `[` has no closing `]`");
        if (p->s[p->i] == ']') { p->i++; break; }
        int32_t v = t_value(p);
        if (v < 0) return -1;
        dt_push(t, v);
        if (!t_ws_nl(p)) return -1;
        if (p->i < p->n && p->s[p->i] == ',') { p->i++; continue; }
        if (p->i < p->n && p->s[p->i] == ']') { p->i++; break; }
        return t_err(p, p->i, p->i >= p->n ? "a `[` has no closing `]`" : "expected `,` or `]`");
    }
    dt_close(t, id, mark);
    t->nodes[id].slen = p->i - at;
    p->depth--;
    return id;
}

static int32_t t_inline(tp *p) {
    int64_t at = p->i++;
    if (++p->depth > DT_MAX_DEPTH) return t_err(p, at, "values are nested more than 500 deep");
    int32_t tab = t_new_table(p, TF_INLINE, 0, at);
    if (tab < 0) return -1;
    bool first = true;
    for (;;) {
        if (!t_ws_nl(p)) return -1;
        if (p->i >= p->n) return t_err(p, at, "a `{` has no closing `}`");
        if (p->s[p->i] == '}') {
            p->i++;
            break;
        }
        if (!first && p->s[p->i] == ',') return t_err(p, p->i, "a value is missing before `,`");
        if (!t_keyval(p, tab, TF_DOTTED | TF_INLINE)) return -1;
        first = false;
        if (!t_ws_nl(p)) return -1;
        if (p->i < p->n && p->s[p->i] == ',') { p->i++; continue; }
        if (p->i < p->n && p->s[p->i] == '}') { p->i++; break; }
        return t_err(p, p->i, p->i >= p->n ? "a `{` has no closing `}`" : "expected `,` or `}`");
    }
    p->depth--;
    int32_t id = t_build(p, tab);
    if (id >= 0) p->t->nodes[id].slen = p->i - at;
    return id;
}

static int32_t t_value(tp *p) {
    if (p->i >= p->n) return t_err(p, p->i, "expected a value");
    uint8_t c = p->s[p->i];
    int32_t id;
    if (c == '"') id = t_basic(p, t_c(p, 1) == '"' && t_c(p, 2) == '"');
    else if (c == '\'') id = t_literal(p, t_c(p, 1) == '\'' && t_c(p, 2) == '\'');
    else if (c == '[') return t_array(p);
    else if (c == '{') return t_inline(p);
    else if (c == 't' || c == 'f') {
        bool b = c == 't';
        int64_t k = b ? 4 : 5;
        if (p->n - p->i < k || memcmp(p->s + p->i, b ? "true" : "false", (size_t)k) != 0 || !t_delim(p, p->i + k)) return t_err(p, p->i, "expected a value (strings are quoted)");
        id = dt_node(p->t, DN_BOOL, p->i);
        p->t->nodes[id].v.b = b;
        p->t->nodes[id].slen = k;
        p->i += k;
        return id;
    } else if (t_digit(c) || c == '+' || c == '-' || c == 'i' || c == 'n') return t_number(p);
    else return t_err(p, p->i, "expected a value (strings are quoted)");
    if (id >= 0 && !t_delim(p, p->i)) return t_err(p, p->i, "unexpected text after a string");
    return id;
}

// `key = value` into table `tab`; the tables a dotted key makes get
// `dotted_flags`.
static bool t_keyval(tp *p, int32_t tab, uint8_t dotted_flags) {
    int64_t base = p->nkb;
    int nk = t_key(p);
    if (nk < 0) return false;
#define KEY(k) (p->kb[base + (k)])
    t_ws(p);
    if (t_c(p, 0) != '=') return t_err(p, p->i, "expected `=` after the key") >= 0;
    p->i++;
    t_ws(p);
    int32_t cur = tab;
    for (int k = 0; k < nk - 1; k++) {
        int32_t e = t_lookup(p, cur, KEY(k));
        if (e < 0) {
            int32_t nt = t_new_table(p, dotted_flags, p->tabs[cur].depth + 1, p->t->nodes[KEY(k)].src);
            if (nt < 0) return false;
            t_add(p, cur, KEY(k), TV_TAB, nt);
            cur = nt;
            continue;
        }
        tent *en = &p->tabs[cur].e[e];
        if (en->kind == TV_TAB && (p->tabs[en->ref].flags & TF_DOTTED)) {
            cur = en->ref;
            continue;
        }
        dt_fail_str(p->d, p->t->nodes[KEY(k)].src, "the key `", dt_strv(p->t, &p->t->nodes[KEY(k)]), en->kind == TV_NODE ? "` already has a value" : "` is a table defined elsewhere: dotted keys can't add to it");
        return false;
    }
    if (t_lookup(p, cur, KEY(nk - 1)) >= 0) {
        dt_fail_str(p->d, p->t->nodes[KEY(nk - 1)].src, "the key `", dt_strv(p->t, &p->t->nodes[KEY(nk - 1)]), "` is defined twice");
        return false;
    }
    int32_t last = KEY(nk - 1);
    p->nkb = base;
#undef KEY
    int32_t v = t_value(p);
    if (v < 0) return false;
    t_add(p, cur, last, TV_NODE, v);
    return true;
}

// `[a.b]` or `[[a.b]]`: the table the lines after it go into, or -1.
static int32_t t_header(tp *p) {
    int64_t at = p->i;
    bool aot = t_c(p, 1) == '[';
    p->i += aot ? 2 : 1;
    t_ws(p);
    int64_t kat = p->i, base = p->nkb;
    int nk = t_key(p);
    if (nk < 0) return -1;
    p->nkb = base;
    int32_t *keys = p->kb + base;
    gojo_str name = { p->s + kat, p->i - kat };
    while (name.len && (name.ptr[name.len - 1] == ' ' || name.ptr[name.len - 1] == '\t')) name.len--;
    t_ws(p);
    if (t_c(p, 0) != ']' || (aot && t_c(p, 1) != ']')) return t_err(p, p->i, aot ? "expected `]]`" : "expected `]`");
    p->i += aot ? 2 : 1;
    if (!t_eol(p)) return -1;
    int32_t cur = 0;
    for (int k = 0; k < nk - 1; k++) {
        int32_t e = t_lookup(p, cur, keys[k]);
        if (e < 0) {
            int32_t nt = t_new_table(p, TF_IMPLICIT, p->tabs[cur].depth + 1, p->t->nodes[keys[k]].src);
            if (nt < 0) return -1;
            t_add(p, cur, keys[k], TV_TAB, nt);
            cur = nt;
            continue;
        }
        tent *en = &p->tabs[cur].e[e];
        if (en->kind == TV_TAB) cur = en->ref;
        else if (en->kind == TV_AOT) cur = p->aots[en->ref].tabs[p->aots[en->ref].n - 1];
        else {
            dt_fail_str(p->d, at, "the key `", dt_strv(p->t, &p->t->nodes[keys[k]]), "` already has a value, so it can't hold a table");
            return -1;
        }
    }
    int32_t last = keys[nk - 1];
    int32_t e = t_lookup(p, cur, last);
    if (aot) {
        int32_t nt = t_new_table(p, TF_EXPLICIT, p->tabs[cur].depth + 1, at);
        if (nt < 0) return -1;
        if (e < 0) {
            int32_t a = t_new_aot(p);
            t_aot_push(p, a, nt);
            t_add(p, cur, last, TV_AOT, a);
            return nt;
        }
        tent *en = &p->tabs[cur].e[e];
        if (en->kind == TV_AOT) {
            t_aot_push(p, en->ref, nt);
            return nt;
        }
        dt_fail_str(p->d, at, "[[", name, en->kind == TV_TAB ? "]]: the key is already a table" : "]]: the key already has a value");
        return -1;
    }
    if (e < 0) {
        int32_t nt = t_new_table(p, TF_EXPLICIT, p->tabs[cur].depth + 1, at);
        if (nt < 0) return -1;
        t_add(p, cur, last, TV_TAB, nt);
        return nt;
    }
    tent *en = &p->tabs[cur].e[e];
    if (en->kind == TV_TAB) {
        ttab *tb = &p->tabs[en->ref];
        if (tb->flags & TF_EXPLICIT) { dt_fail_str(p->d, at, "the table [", name, "] is defined twice"); return -1; }
        if (tb->flags & TF_DOTTED) { dt_fail_str(p->d, at, "the table [", name, "] was already defined by dotted keys"); return -1; }
        tb->flags |= TF_EXPLICIT;
        tb->src = at;
        return en->ref;
    }
    dt_fail_str(p->d, at, "[", name, en->kind == TV_AOT ? "]: the key is an array of tables (write [[...]])" : "]: the key already has a value");
    return -1;
}

static int32_t t_build(tp *p, int32_t tab) {
    gojo_dtree *t = p->t;
    int32_t id = dt_node(t, DN_MAP, p->tabs[tab].src);
    int64_t mark = t->nt;
    for (int32_t k = 0; k < p->tabs[tab].n; k++) {
        tent en = p->tabs[tab].e[k];
        int32_t v;
        if (en.kind == TV_NODE) v = en.ref;
        else if (en.kind == TV_TAB) v = t_build(p, en.ref);
        else {
            v = dt_node(t, DN_SEQ, p->tabs[p->aots[en.ref].tabs[0]].src);
            int64_t m2 = t->nt;
            for (int32_t j = 0; j < p->aots[en.ref].n; j++) dt_push(t, t_build(p, p->aots[en.ref].tabs[j]));
            dt_close(t, v, m2);
        }
        dt_push(t, en.key);
        dt_push(t, v);
    }
    dt_close(t, id, mark);
    return id;
}

static bool toml_parse(gojo_jdec *d, gojo_dtree *t) {
    tp p = { 0 };
    p.d = d;
    p.t = t;
    p.s = t->src;
    p.n = t->len;
    bool ok = false;
    if (p.n >= 3 && p.s[0] == 0xEF && p.s[1] == 0xBB && p.s[2] == 0xBF) p.i = 3;
    int32_t cur = t_new_table(&p, TF_EXPLICIT, 0, p.i);
    for (;;) {
        t_ws(&p);
        if (p.i >= p.n) break;
        uint8_t c = p.s[p.i];
        if (c == '\n' || c == '\r' || c == '#') {
            if (!t_eol(&p)) goto done;
            continue;
        }
        if (c == '[') {
            cur = t_header(&p);
            if (cur < 0) goto done;
            continue;
        }
        if (!t_keyval(&p, cur, TF_DOTTED)) goto done;
        if (!t_eol(&p)) goto done;
    }
    t->root = t_build(&p, 0);
    ok = true;
done:
    for (int64_t k = 0; k < p.ntab; k++) free(p.tabs[k].e);
    for (int64_t k = 0; k < p.naot; k++) free(p.aots[k].tabs);
    free(p.tabs);
    free(p.aots);
    free(p.h);
    free(p.kb);
    return ok;
}

// ------------------------------------------------------------------ entry points for decoding

bool gojo_tdec_init(gojo_jdec *d, gojo_str s, int fmt, bool all) {
    gojo_jdec_init(d, s);
    gojo_dtree *t = gojo_realloc(NULL, sizeof *t);
    memset(t, 0, sizeof *t);
    t->src = s.ptr;
    t->len = s.len;
    t->fmt = fmt;
    d->t = t;
    bool isctrl;
    int64_t bad = dt_bad_text(s.ptr, s.len, fmt == GOJO_FMT_YAML, &isctrl);
    if (bad >= 0) return dt_fail(d, bad, isctrl ? "a control character in the text (write it as an escape in a double-quoted string: \"\\x01\")" : "the text isn't valid UTF-8");
    bool ok = fmt == GOJO_FMT_YAML ? yaml_parse(d, t, all) : toml_parse(d, t);
    if (!ok) return false;
    td_at(d, t->root);
    return true;
}

// ------------------------------------------------------------------ writing YAML and TOML

typedef struct {
    gojo_dtree *t;
    gojo_string *o;
    gojo_string err;
    gojo_string path;
} de;

static void de_spaces(gojo_string *o, int64_t n) {
    static const char sp[] = "                                                                ";
    const int64_t k = (int64_t)sizeof sp - 1;
    while (n > 0) {
        gojo_str c = { (const uint8_t *)sp, n < k ? n : k };
        gojo_string_push(o, c);
        n -= c.len;
    }
}

// A float that reads back as one: always with a `.` (1.0, 1.0e+21).
static void de_float(gojo_string *o, double v, bool toml) {
    if (isnan(v)) { gojo_fmt_cstr(o, toml ? "nan" : ".nan"); return; }
    if (isinf(v)) { gojo_fmt_cstr(o, v > 0 ? (toml ? "inf" : ".inf") : (toml ? "-inf" : "-.inf")); return; }
    int64_t st = o->len;
    gojo_fmt_f64(o, v);
    int64_t e = -1;
    for (int64_t k = st; k < o->len; k++) {
        if (o->ptr[k] == '.') return;
        if (o->ptr[k] == 'e' || o->ptr[k] == 'E') { e = k; break; }
    }
    if (e < 0) { gojo_fmt_cstr(o, ".0"); return; }
    gojo_String_reserve(o, 2);
    memmove(o->ptr + e + 2, o->ptr + e, (size_t)(o->len - e));
    o->ptr[e] = '.';
    o->ptr[e + 1] = '0';
    o->len += 2;
}

static void de_int(de *e, const dnode *n) {
    if (n->slen) gojo_string_push(e->o, dt_text(e->t, n));
    else if (n->flags & DF_U64) gojo_fmt_u64(e->o, n->v.u);
    else gojo_fmt_i64(e->o, n->v.i);
}

// A float of the tree: the encoder's text when there is one (it has a `.`
// or an exponent; YAML 1.1 wants a `.` before the exponent too).
static void de_float_node(de *e, const dnode *n, bool toml) {
    if (!n->slen || !isfinite(n->v.f)) { de_float(e->o, n->v.f, toml); return; }
    gojo_str t = dt_text(e->t, n);
    int64_t ex = -1;
    for (int64_t k = 0; k < t.len; k++) {
        if (t.ptr[k] == '.') { ex = -1; break; }
        if (t.ptr[k] == 'e' || t.ptr[k] == 'E') ex = k;
    }
    if (ex < 0) { gojo_string_push(e->o, t); return; }
    gojo_str a = { t.ptr, ex }, b = { t.ptr + ex, t.len - ex };
    gojo_string_push(e->o, a);
    gojo_fmt_cstr(e->o, ".0");
    gojo_string_push(e->o, b);
}

// The code point at s[i] (its length in *k), or -1 for invalid UTF-8.
static int32_t de_rune(gojo_str s, int64_t i, int *k) {
    uint8_t c = s.ptr[i];
    if (c < 0x80) { *k = 1; return c; }
    int n = (c & 0xE0) == 0xC0 ? 2 : (c & 0xF0) == 0xE0 ? 3 : (c & 0xF8) == 0xF0 ? 4 : 0;
    *k = 1;
    if (!n || i + n > s.len) return -1;
    uint32_t r = c & (0xFF >> (n + 1));
    for (int j = 1; j < n; j++) {
        if ((s.ptr[i + j] & 0xC0) != 0x80) return -1;
        r = (r << 6) | (s.ptr[i + j] & 0x3F);
    }
    static const uint32_t mins[5] = { 0, 0, 0x80, 0x800, 0x10000 };
    if (r < mins[n] || r > 0x10FFFF || (r >= 0xD800 && r <= 0xDFFF)) return -1;
    *k = n;
    return (int32_t)r;
}

// characters YAML can't show as they are (or that other tools mangle)
static bool de_special(int32_t r) {
    return r < 0 || (r < 0x20 && r != '\n' && r != '\t') || r == 0x7F || (r >= 0x80 && r <= 0x9F) || r == 0x2028 || r == 0x2029 || r == 0xFEFF || r == 0xFFFE || r == 0xFFFF;
}

static void de_hex(gojo_string *o, const char *pre, uint32_t v, int digits) {
    static const char hx[] = "0123456789ABCDEF";
    gojo_fmt_cstr(o, pre);
    for (int k = digits - 1; k >= 0; k--) gojo_string_push_byte(o, (uint8_t)hx[(v >> (4 * k)) & 15]);
}

// a YAML double-quoted string
static void ye_dq(gojo_string *o, gojo_str s) {
    gojo_string_push_byte(o, '"');
    int64_t run = 0;
    for (int64_t i = 0; i < s.len;) {
        uint8_t b = s.ptr[i];
        if (b >= 0x20 && b < 0x7F && b != '"' && b != '\\') { i++; continue; }
        int k;
        int32_t r = de_rune(s, i, &k);
        if (r >= 0xA0 && r != 0x2028 && r != 0x2029 && r != 0xFEFF && r != 0xFFFE && r != 0xFFFF) { i += k; continue; }
        gojo_str chunk = { s.ptr + run, i - run };
        gojo_string_push(o, chunk);
        i += k;
        run = i;
        switch (r) {
        case '"': gojo_fmt_cstr(o, "\\\""); continue;
        case '\\': gojo_fmt_cstr(o, "\\\\"); continue;
        case '\n': gojo_fmt_cstr(o, "\\n"); continue;
        case '\t': gojo_fmt_cstr(o, "\\t"); continue;
        case '\r': gojo_fmt_cstr(o, "\\r"); continue;
        case 0: gojo_fmt_cstr(o, "\\0"); continue;
        case 0x2028: gojo_fmt_cstr(o, "\\L"); continue;
        case 0x2029: gojo_fmt_cstr(o, "\\P"); continue;
        default: break;
        }
        if (r < 0) dt_rune(o, 0xFFFD);
        else if (r < 0x20 || r == 0x7F || (r >= 0x80 && r <= 0x9F)) de_hex(o, "\\x", (uint32_t)r, 2);
        else de_hex(o, "\\u", (uint32_t)r, 4);
    }
    gojo_str chunk = { s.ptr + run, s.len - run };
    gojo_string_push(o, chunk);
    gojo_string_push_byte(o, '"');
}

// Whether a string can be written without quotes and read back as the same
// string, by YAML 1.2 and by YAML 1.1 tools (`yes`, `on`, `0777`, dates...).
static bool ye_plain_ok(gojo_str s) {
    if (!s.len) return false;
    uint8_t c0 = s.ptr[0], c1 = s.len > 1 ? s.ptr[1] : 0, cl = s.ptr[s.len - 1];
    if (cl == ' ' || cl == ':' || cl == '\t') return false;
    if (c0 == '-' || c0 == '.') {
        // `--flag` and `.hidden`, not `---`, `-1`, `.5` or `.inf`
        bool alpha = (c1 >= 'a' && c1 <= 'z') || (c1 >= 'A' && c1 <= 'Z') || c1 == '_';
        if (c0 == '-' && c1 == '-') alpha = !(s.len > 2 && s.ptr[2] == '-');
        if (!alpha) return false;
        if (c0 == '.' && s.len == 4) {
            const uint8_t *w = s.ptr + 1;
            if (y_eq(w, 3, "inf") || y_eq(w, 3, "Inf") || y_eq(w, 3, "INF") || y_eq(w, 3, "nan") || y_eq(w, 3, "NaN") || y_eq(w, 3, "NAN")) return false;
        }
    } else if (c0 >= '0' && c0 <= '9') {
        // `512Mi`, but nothing that YAML 1.1 reads as a number, a time or a
        // date either: a letter that no number has, and no `-` or `:`
        bool letter = false;
        for (int64_t i = 1; i < s.len; i++) {
            uint8_t b = s.ptr[i];
            if (b == '-' || b == ':') return false;
            letter = letter || (((b >= 'g' && b <= 'z') || (b >= 'G' && b <= 'Z')) && !strchr("xXoO", b));
        }
        if (!letter) return false;
    } else if (c0 < 0x80 && strchr("?:,[]{}#&*!|>'\"%@`+~<= \t", c0)) return false;
    if (s.len <= 5 && strchr("yYnNtTfFoO", c0)) {
        static const char *words[] = { "y", "Y", "yes", "Yes", "YES", "n", "N", "no", "No", "NO", "true", "True", "TRUE", "false", "False", "FALSE", "on", "On", "ON", "off", "Off", "OFF", "null", "Null", "NULL" };
        for (size_t w = 0; w < sizeof words / sizeof *words; w++)
            if (y_eq(s.ptr, s.len, words[w])) return false;
    }
    // what YAML 1.2 reads it as: the parser's own rules
    dnode probe = { 0 };
    probe.kind = DN_STR;
    if (!y_scalar(&probe, s) || probe.kind != DN_STR) return false;
    for (int64_t i = 0; i < s.len;) {
        uint8_t b = s.ptr[i];
        if (b >= 0x20 && b < 0x7F) {
            if ((b == ':' || b == ' ') && i + 1 < s.len && s.ptr[i + 1] == (b == ':' ? ' ' : '#')) return false;
            i++;
            continue;
        }
        int k;
        int32_t r = de_rune(s, i, &k);
        if (r == '\n' || r == '\t' || de_special(r)) return false;
        if (r == ':' && i + 1 < s.len && s.ptr[i + 1] == ' ') return false;
        if (r == ' ' && i + 1 < s.len && s.ptr[i + 1] == '#') return false;
        i += k;
    }
    return true;
}

// Whether a string with line breaks can be a literal block scalar (`|`).
static bool ye_block_ok(gojo_str s) {
    bool nl = false, content = false;
    for (int64_t i = 0; i < s.len;) {
        int k;
        int32_t r = de_rune(s, i, &k);
        if (r == '\n') nl = true;
        else if (r == '\r' || de_special(r)) return false;
        else if (r != ' ') content = true;
        i += k;
    }
    return nl && content;
}

static void ye_block(gojo_string *o, gojo_str s, int64_t ind) {
    int64_t end = s.len, trail = 0;
    while (end > 0 && s.ptr[end - 1] == '\n') { end--; trail++; }
    // an indentation indicator when the first line with content starts with
    // a space, or blank lines before it have spaces
    bool indicator = false;
    for (int64_t i = 0; i < end; i++) {
        if (s.ptr[i] == '\n') continue;
        indicator = s.ptr[i] == ' ' || s.ptr[i] == '\t';
        break;
    }
    gojo_string_push_byte(o, '|');
    if (indicator) gojo_string_push_byte(o, '2');
    if (trail == 0) gojo_string_push_byte(o, '-');
    else if (trail > 1) gojo_string_push_byte(o, '+');
    gojo_string_push_byte(o, '\n');
    int64_t i = 0;
    while (i < end) {
        int64_t j = i;
        while (j < end && s.ptr[j] != '\n') j++;
        if (j > i) {
            de_spaces(o, ind);
            gojo_str line = { s.ptr + i, j - i };
            gojo_string_push(o, line);
        }
        gojo_string_push_byte(o, '\n');
        i = j + 1;
    }
    for (int64_t k = 1; k < trail; k++) gojo_string_push_byte(o, '\n');
}

static void ye_str(gojo_string *o, gojo_str s) {
    if (ye_plain_ok(s)) gojo_string_push(o, s);
    else ye_dq(o, s);
}

enum { YE_ROOT, YE_KEY, YE_ITEM };

static void ye_value(de *e, int32_t id, int64_t indent, int ctx);

static void ye_map(de *e, int32_t id, int64_t indent, bool first_inline) {
    dnode *n = &e->t->nodes[id];
    for (int32_t i = 0; i < n->n; i++) {
        int32_t k = e->t->kids[n->v.first + 2 * i], v = e->t->kids[n->v.first + 2 * i + 1];
        if (i > 0 || !first_inline) de_spaces(e->o, indent);
        gojo_str key = dt_strv(e->t, &e->t->nodes[k]);
        if (key.len > 1000) {
            // an implicit key is at most 1024 characters: `? key` then `: value`
            gojo_fmt_cstr(e->o, "? ");
            ye_dq(e->o, key);
            gojo_string_push_byte(e->o, '\n');
            de_spaces(e->o, indent);
        } else if (key.len == 2 && key.ptr[0] == '<' && key.ptr[1] == '<') ye_dq(e->o, key);
        else ye_str(e->o, key);
        gojo_string_push_byte(e->o, ':');
        ye_value(e, v, indent, YE_KEY);
        n = &e->t->nodes[id];
    }
}

static void ye_seq(de *e, int32_t id, int64_t indent, bool first_inline) {
    dnode *n = &e->t->nodes[id];
    for (int32_t i = 0; i < n->n; i++) {
        if (i > 0 || !first_inline) de_spaces(e->o, indent);
        gojo_fmt_cstr(e->o, "- ");
        ye_value(e, e->t->kids[n->v.first + i], indent, YE_ITEM);
        n = &e->t->nodes[id];
    }
}

static void ye_value(de *e, int32_t id, int64_t indent, int ctx) {
    dnode *n = &e->t->nodes[id];
    if ((n->kind == DN_MAP || n->kind == DN_SEQ) && n->n > 0) {
        if (ctx == YE_KEY) gojo_string_push_byte(e->o, '\n');
        int64_t in = ctx == YE_ROOT ? 0 : indent + 2;
        if (n->kind == DN_MAP) ye_map(e, id, in, ctx == YE_ITEM);
        else ye_seq(e, id, in, ctx == YE_ITEM);
        return;
    }
    if (ctx == YE_KEY) gojo_string_push_byte(e->o, ' ');
    switch (n->kind) {
    case DN_NULL: gojo_fmt_cstr(e->o, "null"); break;
    case DN_BOOL: gojo_fmt_cstr(e->o, n->v.b ? "true" : "false"); break;
    case DN_INT: de_int(e, n); break;
    case DN_FLOAT: de_float_node(e, n, false); break;
    case DN_SEQ: gojo_fmt_cstr(e->o, "[]"); break;
    case DN_MAP: gojo_fmt_cstr(e->o, "{}"); break;
    default: {
        gojo_str s = dt_strv(e->t, n);
        if (ye_block_ok(s)) {
            ye_block(e->o, s, ctx == YE_ROOT ? 2 : indent + 2);
            return;
        }
        ye_str(e->o, s);
    }
    }
    gojo_string_push_byte(e->o, '\n');
}

// yaml.encode: compact JSON from the generated encoder, as YAML. False when
// it is nested more than DT_MAX_DEPTH deep.
bool gojo_yaml_emit(gojo_str json, gojo_string *out) {
    gojo_dtree *t = jr_tree(json, GOJO_FMT_YAML);
    if (!t) return false;
    de e = { t, out, { 0 }, { 0 } };
    ye_value(&e, t->root, 0, YE_ROOT);
    gojo_dtree_free(t);
    return true;
}

// a TOML basic string
static void te_str(gojo_string *o, gojo_str s) {
    gojo_string_push_byte(o, '"');
    int64_t run = 0;
    for (int64_t i = 0; i < s.len;) {
        uint8_t b = s.ptr[i];
        if (b >= 0x20 && b < 0x7F && b != '"' && b != '\\') { i++; continue; }
        int k;
        int32_t r = de_rune(s, i, &k);
        if (r >= 0x80) { i += k; continue; }
        gojo_str chunk = { s.ptr + run, i - run };
        gojo_string_push(o, chunk);
        i += k;
        run = i;
        switch (r) {
        case '"': gojo_fmt_cstr(o, "\\\""); continue;
        case '\\': gojo_fmt_cstr(o, "\\\\"); continue;
        case '\n': gojo_fmt_cstr(o, "\\n"); continue;
        case '\t': gojo_fmt_cstr(o, "\\t"); continue;
        case '\r': gojo_fmt_cstr(o, "\\r"); continue;
        case 8: gojo_fmt_cstr(o, "\\b"); continue;
        case 12: gojo_fmt_cstr(o, "\\f"); continue;
        default: break;
        }
        if (r < 0) dt_rune(o, 0xFFFD);
        else de_hex(o, "\\u", (uint32_t)r, 4);
    }
    gojo_str chunk = { s.ptr + run, s.len - run };
    gojo_string_push(o, chunk);
    gojo_string_push_byte(o, '"');
}

// a string with line breaks as a multi-line literal string, when it can be one
static bool te_multiline(gojo_string *o, gojo_str s) {
    bool nl = false;
    for (int64_t i = 0; i < s.len;) {
        int k;
        int32_t r = de_rune(s, i, &k);
        if (r == '\n') nl = true;
        else if (r < 0 || (r < 0x20 && r != '\t') || r == 0x7F) return false;
        if (r == '\'' && i + 2 < s.len && s.ptr[i + 1] == '\'' && s.ptr[i + 2] == '\'') return false;
        i += k;
    }
    if (!nl) return false;
    gojo_fmt_cstr(o, "'''\n");
    gojo_string_push(o, s);
    gojo_fmt_cstr(o, "'''");
    return true;
}

static void te_key(gojo_string *o, gojo_str k) {
    bool bare = k.len > 0;
    for (int64_t i = 0; i < k.len && bare; i++) {
        uint8_t c = k.ptr[i];
        bare = (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9') || c == '_' || c == '-';
    }
    if (bare) gojo_string_push(o, k);
    else te_str(o, k);
}

static int64_t de_path_key(de *e, gojo_str k) {
    int64_t mark = e->path.len;
    bool ident = k.len > 0;
    for (int64_t i = 0; i < k.len; i++) {
        uint8_t c = k.ptr[i];
        ident = ident && ((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || c == '_' || (i > 0 && c >= '0' && c <= '9'));
    }
    if (ident) {
        gojo_string_push_byte(&e->path, '.');
        gojo_string_push(&e->path, k);
    } else {
        gojo_string_push_byte(&e->path, '[');
        gojo_jenc_str(&e->path, k);
        gojo_string_push_byte(&e->path, ']');
    }
    return mark;
}

static int64_t de_path_index(de *e, int64_t i) {
    int64_t mark = e->path.len;
    gojo_string_push_byte(&e->path, '[');
    gojo_fmt_i64(&e->path, i);
    gojo_string_push_byte(&e->path, ']');
    return mark;
}

static bool te_fail(de *e, const char *msg) {
    if (e->err.len) return false;
    gojo_fmt_cstr(&e->err, msg);
    if (e->path.len) {
        gojo_fmt_cstr(&e->err, " (in ");
        gojo_string_push(&e->err, gojo_string_view(&e->path));
        gojo_fmt_cstr(&e->err, ")");
    }
    return false;
}

static bool te_inline(de *e, int32_t id) {
    dnode *n = &e->t->nodes[id];
    switch (n->kind) {
    case DN_NULL: return te_fail(e, "TOML has no null (leave the key out, or use a value)");
    case DN_BOOL: gojo_fmt_cstr(e->o, n->v.b ? "true" : "false"); return true;
    case DN_INT:
        if (n->flags & (DF_U64 | DF_BIG)) return te_fail(e, "the integer doesn't fit in TOML's 64-bit integers");
        gojo_fmt_i64(e->o, n->v.i);
        return true;
    case DN_FLOAT: de_float_node(e, n, true); return true;
    case DN_STR: {
        gojo_str s = dt_strv(e->t, n);
        if (!te_multiline(e->o, s)) te_str(e->o, s);
        return true;
    }
    case DN_SEQ: {
        gojo_string_push_byte(e->o, '[');
        for (int32_t i = 0; i < n->n; i++) {
            if (i) gojo_fmt_cstr(e->o, ", ");
            int64_t m = de_path_index(e, i);
            if (!te_inline(e, e->t->kids[e->t->nodes[id].v.first + i])) return false;
            e->path.len = m;
        }
        gojo_string_push_byte(e->o, ']');
        return true;
    }
    default: {
        bool first = true;
        gojo_string_push_byte(e->o, '{');
        for (int32_t i = 0; i < n->n; i++) {
            int32_t k = e->t->kids[e->t->nodes[id].v.first + 2 * i], v = e->t->kids[e->t->nodes[id].v.first + 2 * i + 1];
            if (e->t->nodes[v].kind == DN_NULL) continue;
            gojo_fmt_cstr(e->o, first ? " " : ", ");
            first = false;
            gojo_str key = dt_strv(e->t, &e->t->nodes[k]);
            te_key(e->o, key);
            gojo_fmt_cstr(e->o, " = ");
            int64_t m = de_path_key(e, key);
            if (!te_inline(e, v)) return false;
            e->path.len = m;
        }
        gojo_fmt_cstr(e->o, first ? "}" : " }");
        return true;
    }
    }
}

// how a value of a table is written: 0 left out (null), 1 `key = value`,
// 2 a table of its own, 3 an array of tables
static int te_kind(de *e, int32_t v) {
    dnode *n = &e->t->nodes[v];
    if (n->kind == DN_NULL) return 0;
    if (n->kind == DN_MAP && n->n > 0) return 2;
    if (n->kind == DN_SEQ && n->n > 0) {
        for (int32_t i = 0; i < n->n; i++)
            if (e->t->nodes[e->t->kids[n->v.first + i]].kind != DN_MAP) return 1;
        return 3;
    }
    return 1;
}

static bool te_table(de *e, int32_t id, gojo_string *hdr) {
    gojo_dtree *t = e->t;
    int32_t np = t->nodes[id].n;
    int64_t first = t->nodes[id].v.first;
    for (int32_t i = 0; i < np; i++) {
        int32_t k = t->kids[first + 2 * i], v = t->kids[first + 2 * i + 1];
        if (te_kind(e, v) != 1) continue;
        gojo_str key = dt_strv(t, &t->nodes[k]);
        te_key(e->o, key);
        gojo_fmt_cstr(e->o, " = ");
        int64_t m = de_path_key(e, key);
        if (!te_inline(e, v)) return false;
        e->path.len = m;
        gojo_string_push_byte(e->o, '\n');
    }
    for (int pass = 2; pass <= 3; pass++) {
        for (int32_t i = 0; i < np; i++) {
            int32_t k = t->kids[first + 2 * i], v = t->kids[first + 2 * i + 1];
            if (te_kind(e, v) != pass) continue;
            gojo_str key = dt_strv(t, &t->nodes[k]);
            int64_t hl = hdr->len;
            if (hl) gojo_string_push_byte(hdr, '.');
            te_key(hdr, key);
            int64_t m = de_path_key(e, key);
            int32_t nitems = pass == 2 ? 1 : t->nodes[v].n;
            for (int32_t j = 0; j < nitems; j++) {
                int32_t sub = pass == 2 ? v : t->kids[t->nodes[v].v.first + j];
                int64_t m2 = pass == 3 ? de_path_index(e, j) : e->path.len;
                // a table holding only tables gets its header from theirs
                bool header = pass == 3;
                if (!header) {
                    bool own = false, deeper = false;
                    for (int32_t q = 0; q < t->nodes[sub].n; q++) {
                        int kk = te_kind(e, t->kids[t->nodes[sub].v.first + 2 * q + 1]);
                        if (kk == 1) own = true;
                        else if (kk >= 2) deeper = true;
                    }
                    header = own || !deeper;
                }
                if (header) {
                    if (e->o->len) gojo_string_push_byte(e->o, '\n');
                    gojo_fmt_cstr(e->o, pass == 3 ? "[[" : "[");
                    gojo_string_push(e->o, gojo_string_view(hdr));
                    gojo_fmt_cstr(e->o, pass == 3 ? "]]\n" : "]\n");
                }
                if (!te_table(e, sub, hdr)) return false;
                e->path.len = m2;
            }
            e->path.len = m;
            hdr->len = hl;
        }
    }
    return true;
}

// toml.encode: compact JSON from the generated encoder, as TOML; false with
// the reason in *err when TOML can't hold it.
bool gojo_toml_emit(gojo_str json, gojo_string *out, gojo_string *err) {
    gojo_dtree *t = jr_tree(json, GOJO_FMT_TOML);
    if (!t) {
        gojo_fmt_cstr(err, "values are nested more than 500 deep");
        return false;
    }
    de e = { t, out, { 0 }, { 0 } };
    gojo_string hdr = { 0 };
    bool ok;
    dnode *root = &t->nodes[t->root];
    if (root->kind != DN_MAP) {
        gojo_fmt_cstr(&e.err, "a TOML document is a table: encode a struct or a map, not ");
        gojo_fmt_cstr(&e.err, td_kind_name(t, root));
        ok = false;
    } else ok = te_table(&e, t->root, &hdr);
    if (!ok) {
        gojo_string_push(err, gojo_string_view(&e.err));
        out->len = 0;
    }
    free(e.err.ptr);
    free(e.path.ptr);
    free(hdr.ptr);
    gojo_dtree_free(t);
    return ok;
}
