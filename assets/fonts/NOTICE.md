# Embedded font

BNAuth embeds one font file for Latin, Cyrillic and CJK text: **Nowar Sans TW Regular**, weight 400, version 1.0.1, from the standard Nowar Sans release. It is not the Dragonflight edition or the DINish/Nowar composite used by oUF_Hankk.

- Bundled file: `NowarSansTW-Regular.ttf`.
- Original archive member: `Fonts/bHEI00M.ttf`.
- Source release: https://github.com/nowar-fonts/Nowar-Sans/releases/tag/v1.0.1
- Download: https://github.com/nowar-fonts/Nowar-Sans/releases/download/v1.0.1/NowarSans-Bliz-400-1.0.1.7z
- Archive size: 9,018,232 bytes.
- Archive SHA-256: `0ffa1bb257b2a8fd8b2237c0c6deb4c0de000fe63c93f5091128d1bcf187c20f`.
- Font size: 9,786,792 bytes.
- Font SHA-256: `f8ba0714e49aaab4680c5bc81ce01066396be326f727f3d0263255102f319940`.
- Format: static OpenType CFF (`OTTO`), UPEM 1000; 49,463 glyphs and 47,123 Unicode code points.

Only the bundled filename changes. The font bytes, internal names, outlines and glyph set are unchanged; BNAuth does not convert, merge or subset this file. Both egui font families use this same file, and the `default_fonts` feature is disabled. The digits 0-9 each have a 522-unit advance; this does not make every character in the font monospaced.

The TC edition supplies traditional Chinese forms for shared Han characters. Local metadata checks found the tested Latin, Cyrillic, Greek, traditional/simplified Chinese, Japanese kana and Korean samples, including all 11,172 modern Hangul syllables. This is not a claim of complete Unicode coverage. BNAuth draws its three-dot action icon directly, so it does not require a U+22EE glyph.

## Copyright and license

`OFL-NowarSans.txt` is an unmodified copy of the release archive's `Fonts/LICENSE.txt`, including its copyright notices, Reserved Font Names and complete SIL Open Font License 1.1.

The upstream license states:

> Copyright © 2018—2020 Cyano Hao and Nowar Typeface, with Reserved Font Name “Nowar”, “Новар”, “Νοωαρ”, “有爱”, and “有愛”.
>
> Portions Copyright 2015 Google Inc.
>
> Portions © 2014-2019 Adobe (http://www.adobe.com/), with Reserved Font Name 'Source'. Source is a trademark of Adobe in the United States and/or other countries.

The unchanged font metadata additionally states:

> Copyright © 2018—2021 Cyano Hao and Nowar Typeface, with Reserved Font Name “Nowar”, “Новар”, “Νοωαρ”, “有爱”, and “有愛”. Portions Copyright 2015 Google LLC.. Portions © 2014-2021 Adobe (http://www.adobe.com/), with Reserved Font Name 'Source'.

The font remains under OFL-1.1, separate from BNAuth's MIT application license.
