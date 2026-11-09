// Force-included into every grammar by build.rs: routes the scanners' wide-character classification to the tables
// of grammars/wctype.c, which reproduce the musl functions the WASM grammars of the TS reference link against.
#ifndef OPENCODE_MARKUP_WCTYPE_H
#define OPENCODE_MARKUP_WCTYPE_H
#include <wctype.h>
#include <wchar.h>

int opencode_markup_iswalnum(wint_t c);
int opencode_markup_iswalpha(wint_t c);
int opencode_markup_iswblank(wint_t c);
int opencode_markup_iswdigit(wint_t c);
int opencode_markup_iswlower(wint_t c);
int opencode_markup_iswspace(wint_t c);
int opencode_markup_iswupper(wint_t c);
int opencode_markup_iswxdigit(wint_t c);
wint_t opencode_markup_towlower(wint_t c);
wint_t opencode_markup_towupper(wint_t c);

#ifndef OPENCODE_MARKUP_WCTYPE_IMPL
#define iswalnum opencode_markup_iswalnum
#define iswalpha opencode_markup_iswalpha
#define iswblank opencode_markup_iswblank
#define iswdigit opencode_markup_iswdigit
#define iswlower opencode_markup_iswlower
#define iswspace opencode_markup_iswspace
#define iswupper opencode_markup_iswupper
#define iswxdigit opencode_markup_iswxdigit
#define towlower opencode_markup_towlower
#define towupper opencode_markup_towupper
#endif
#endif
