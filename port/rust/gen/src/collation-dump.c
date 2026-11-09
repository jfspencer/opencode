// Dumps ICU's root collation for gen/src/collation.ts, which compiles and runs this file against the system's ICU:
//   cc collation-dump.c -licui18n -licuuc -licudata && ./a.out
//
// With `--ces`, it prints the elements of each line of stdin instead (see elements_of_lines).
//
// Output lines (code points in hex):
//   # icu <version> / # unicode <version>
//   N <cp> <ccc> ; [<NFD of cp>]      every code point with a non-zero combining class or a canonical decomposition
//   C <cp> ; <ces>                    the collation elements of each code point on its own (every scalar value)
//   X <cps> ; <ces>                   each contraction of the root collation and its elements
//   P <cps> ; <ces>                   each prefix (context) mapping, as prefix + code point, and the elements of the
//                                     whole string
// A collation element is <primary>:<secondary>:<tertiary>, the 32-bit primary and the 16-bit secondary and tertiary
// of ICU's 64-bit CE (the tertiary with its case bits, without the quaternary bits), joined from the two old-style
// 32-bit halves CollationElementIterator hands out (collationelementiterator.cpp `next`).
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unicode/ucol.h>
#include <unicode/ucoleitr.h>
#include <unicode/uset.h>
#include <unicode/unorm2.h>
#include <unicode/uchar.h>
#include <unicode/ustring.h>
#include <unicode/uversion.h>

static UCollator *coll;
static UCollationElements *it;

static void die(UErrorCode s, const char *what) {
  if (U_FAILURE(s)) {
    fprintf(stderr, "%s: %s\n", what, u_errorName(s));
    exit(1);
  }
}

static int32_t held;
static int holding;

// The next 64-bit element, joined from its first half and, when the next half is a continuation (low byte 0xc0:
// the primary's low 16 bits, the secondary's and the tertiary's low bytes), from that; 0 at the end.
static int next_element(uint32_t *p, uint32_t *sec, uint32_t *ter) {
  UErrorCode s = U_ZERO_ERROR;
  int32_t ce = holding ? held : ucol_next(it, &s);
  holding = 0;
  die(s, "ucol_next");
  if (ce == UCOL_NULLORDER) return 0;
  uint32_t u = (uint32_t)ce;
  *p = u & 0xffff0000;
  *sec = ((u >> 8) & 0xff) << 8;
  *ter = (u & 0xff) << 8;
  int32_t next = ucol_next(it, &s);
  die(s, "ucol_next");
  if (next != UCOL_NULLORDER && ((uint32_t)next & 0xc0) == 0xc0) {
    *p |= (uint32_t)next >> 16;
    *sec |= ((uint32_t)next >> 8) & 0xff;
    *ter |= (uint32_t)next & 0x3f;
  } else {
    held = next;
    holding = 1;
  }
  return 1;
}

// ICU's iteration can loop forever on some strings (it keeps re-reading a code point after a failed discontiguous
// contraction backs up across a normalized segment), so at most 31 elements per code unit, plus 64, are printed,
// then `+` when there are more (collate.rs's `debug_elements` stops at the same count).
static void ces(const UChar *text, int32_t len) {
  UErrorCode s = U_ZERO_ERROR;
  ucol_setText(it, text, len, &s);
  die(s, "ucol_setText");
  holding = 0;
  uint32_t p, sec, ter;
  for (int32_t printed = 0; next_element(&p, &sec, &ter); printed++) {
    if (printed == 31 * len + 64) {
      printf(" +");
      return;
    }
    printf(" %x:%x:%x", p, sec, ter);
  }
}

static void dump_string(const char *tag, const UChar *text, int32_t len) {
  printf("%s", tag);
  UChar32 c;
  int32_t i = 0;
  while (i < len) {
    U16_NEXT(text, i, len, c);
    printf(" %X", c);
  }
  printf(" ;");
  ces(text, len);
  printf("\n");
}

static void dump_set(const char *tag, USet *set) {
  UErrorCode s = U_ZERO_ERROR;
  int32_t n = uset_getItemCount(set);
  UChar buf[256];
  for (int32_t k = 0; k < n; k++) {
    UChar32 start, end;
    int32_t len = uset_getItem(set, k, &start, &end, buf, 256, &s);
    die(s, "uset_getItem");
    if (len == 0) {
      for (UChar32 c = start; c <= end; c++) {
        UChar one[2];
        int32_t l = 0;
        U16_APPEND_UNSAFE(one, l, c);
        dump_string(tag, one, l);
      }
    } else {
      dump_string(tag, buf, len);
    }
  }
}

// `--ces`: the elements of each line of stdin (hex code points separated by spaces), one line each, for the
// harness (crates/opencode/harness/collate).
static int elements_of_lines(void) {
  char line[1 << 16];
  UChar text[1 << 14];
  while (fgets(line, sizeof line, stdin)) {
    line[strcspn(line, "\n")] = 0;
    int32_t n = 0;
    for (char *token = strtok(line, " "); token; token = strtok(NULL, " ")) {
      UChar32 c = (UChar32)strtol(token, NULL, 16);
      U16_APPEND_UNSAFE(text, n, c);
    }
    ces(text, n);
    printf("\n");
  }
  return 0;
}

int main(int argc, char **argv) {
  UErrorCode s = U_ZERO_ERROR;
  UVersionInfo v;
  char text[U_MAX_VERSION_STRING_LENGTH];
  if (argc > 1 && strcmp(argv[1], "--ces") == 0) {
    coll = ucol_open("", &s);
    ucol_setAttribute(coll, UCOL_NORMALIZATION_MODE, UCOL_ON, &s);
    it = ucol_openElements(coll, NULL, 0, &s);
    die(s, "ucol_openElements");
    return elements_of_lines();
  }
  u_getVersion(v);
  u_versionToString(v, text);
  printf("# icu %s\n", text);
  u_getUnicodeVersion(v);
  u_versionToString(v, text);
  printf("# unicode %s\n", text);
  coll = ucol_open("", &s);
  die(s, "ucol_open");
  ucol_setAttribute(coll, UCOL_NORMALIZATION_MODE, UCOL_ON, &s);
  die(s, "ucol_setAttribute");
  it = ucol_openElements(coll, NULL, 0, &s);
  die(s, "ucol_openElements");
  const UNormalizer2 *nfd = unorm2_getNFDInstance(&s);
  die(s, "unorm2_getNFDInstance");
  for (UChar32 c = 0; c <= 0x10FFFF; c++) {
    if (c >= 0xD800 && c <= 0xDFFF) continue;
    UChar one[2];
    int32_t l = 0;
    U16_APPEND_UNSAFE(one, l, c);
    int ccc = u_getCombiningClass(c);
    UChar out[64];
    UErrorCode e = U_ZERO_ERROR;
    int32_t n = unorm2_normalize(nfd, one, l, out, 64, &e);
    die(e, "unorm2_normalize");
    int same = n == l && memcmp(out, one, l * sizeof(UChar)) == 0;
    if (ccc || !same) {
      printf("N %X %d ;", c, ccc);
      if (!same) {
        UChar32 d;
        int32_t i = 0;
        while (i < n) {
          U16_NEXT(out, i, n, d);
          printf(" %X", d);
        }
      }
      printf("\n");
    }
    dump_string("C", one, l);
  }
  USet *contractions = uset_openEmpty();
  ucol_getContractionsAndExpansions(coll, contractions, NULL, 0, &s);
  die(s, "ucol_getContractionsAndExpansions");
  dump_set("X", contractions);
  USet *prefixed = uset_openEmpty();
  ucol_getContractionsAndExpansions(coll, prefixed, NULL, 1, &s);
  die(s, "ucol_getContractionsAndExpansions");
  uset_removeAll(prefixed, contractions);
  dump_set("P", prefixed);
  return 0;
}
