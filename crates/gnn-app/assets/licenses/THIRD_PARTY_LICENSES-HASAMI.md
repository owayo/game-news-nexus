# Third-Party Licenses

hasami の辞書データと、ライブラリに埋め込む文分割の例外表 (`src/sentence/builtin_exceptions.txt`) には、
以下のサードパーティデータが含まれる。例外表を含むもの (hasami をリンクしたバイナリなど) を配布するときに
添える表示は、「文分割の組み込みの例外表」の節と
[`src/sentence/builtin_exceptions.NOTICE`](src/sentence/builtin_exceptions.NOTICE) にまとめた。

配布辞書 (`ipadic.hsd`・`ipadic-neologd.hsd`・`ipadic-neologd-sudachi.hsd` と、それを zstd で圧縮した `.hsd.zst`) は
リポジトリに置かず、リリースの添付ファイルとして配る。このファイルも同じリリースに添付する。辞書を再配布するときは、
このファイルを一緒に配る。

## SudachiDict

Copyright (c) 2017-2023 Works Applications Co., Ltd.

Licensed under the Apache License, Version 2.0.
See [LICENSE-APACHE-2.0](LICENSE-APACHE-2.0) or https://www.apache.org/licenses/LICENSE-2.0.txt
for the full license text.

- Repository: https://github.com/WorksApplications/SudachiDict

hasami の統合辞書 (`ipadic-neologd-sudachi.hsd`) では、SudachiDict の raw 辞書ソース
(small + core、版は `scripts/build-dict.sh` で固定) の語彙データを MeCab IPAdic 互換形式に変換し、
品詞体系を IPAdic に、文脈 ID を IPAdic の left_id/right_id に写している。
変換スクリプト: `scripts/convert_sudachi_raw.py`

SudachiDict には `NOTICE` という名前のファイルは無い。帰属表示はリポジトリの
[`LEGAL`](https://github.com/WorksApplications/SudachiDict/blob/v20260723/LEGAL) (見出しは
LEGAL NOTICE INFORMATION。SudachiDict の配布物にも `LICENSE-2.0.txt` と一緒に入る) にあり、
small_lex が UniDic の一部を、core_lex と notcore_lex が NEologd の一部を含むと書いている。
下の UniDic と NEologd の表示は `LEGAL` から写したもの。raw 辞書の zip は CSV だけで、ライセンスのファイルを含まない。

### UniDic (SudachiDict に内包)

Copyright (c) 2011-2013, The UniDic Consortium
All rights reserved.

SudachiDict の語彙データの一部 (small_lex) は UniDic に由来する。
UniDic は BSD 3-Clause License (SPDX: `BSD-3-Clause`) の下で利用している。

Redistribution and use in source and binary forms, with or without
modification, are permitted provided that the following conditions are
met:

 * Redistributions of source code must retain the above copyright
   notice, this list of conditions and the following disclaimer.

 * Redistributions in binary form must reproduce the above copyright
   notice, this list of conditions and the following disclaimer in the
   documentation and/or other materials provided with the
   distribution.

 * Neither the name of the UniDic Consortium nor the names of its
   contributors may be used to endorse or promote products derived
   from this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS
"AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT
LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR
A PARTICULAR PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT
OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT
LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE,
DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY
THEORY OF LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT
(INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE
OF THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

- https://unidic.ninjal.ac.jp/

### NEologd (SudachiDict に内包)

Copyright (C) 2015-2019 Toshinori Sato (@overlast)

Licensed under the Apache License, Version 2.0.

- https://github.com/neologd/mecab-unidic-neologd

SudachiDict の core_lex に含まれる。NEologd には以下のデータソースが含まれる:

- はてなキーワード一覧ファイル (著作権: 株式会社はてな)
  http://developer.hatena.ne.jp/ja/documents/keyword/misc/catalog
- 郵便番号データ (日本郵便株式会社)
  http://www.post.japanpost.jp/zipcode/dl/readme.html
- 日本全国駅名一覧 (スナフキん氏)
  http://www5a.biglobe.ne.jp/~harako/data/station.htm
- 人名(姓/名)エントリデータ (工藤拓氏)
  http://chasen.org/~taku/software/misc/personal_name.zip

## mecab-ipadic

Copyright 2000, 2001, 2002, 2003 Nara Institute of Science and Technology. All Rights Reserved.

Licensed under the Nara Institute of Science and Technology License (2003) (SPDX: `NAIST-2003`).
IPAdic の辞書 (`ipadic.hsd`, `ipadic-neologd.hsd`, `ipadic-neologd-sudachi.hsd`) の基盤データ。
版は mecab-ipadic 2.7.0-20070801 (`scripts/build-dict.sh` が taku910/mecab の commit で固定する)。

条文は mecab-ipadic に同梱の `COPYING` の全文で、SPDX License List の
[NAIST-2003](https://spdx.org/licenses/NAIST-2003.html) の条文とバイト単位で一致する
(`COPYING` の条文の後ろの行にある 2 バイト `0xF7 0xF7` を除く)。NAIST-2003 は、元の形でも改変したものでも、
すべての写しに著作権表示とそれに続くすべての段落 (ICOT Free Software の条件と NO WARRANTY を含む) を
含めることを求める。

```text
Copyright 2000, 2001, 2002, 2003 Nara Institute of Science
and Technology.  All Rights Reserved.

Use, reproduction, and distribution of this software is permitted.
Any copy of this software, whether in its original form or modified,
must include both the above copyright notice and the following
paragraphs.

Nara Institute of Science and Technology (NAIST),
the copyright holders, disclaims all warranties with regard to this
software, including all implied warranties of merchantability and
fitness, in no event shall NAIST be liable for
any special, indirect or consequential damages or any damages
whatsoever resulting from loss of use, data or profits, whether in an
action of contract, negligence or other tortuous action, arising out
of or in connection with the use or performance of this software.

A large portion of the dictionary entries
originate from ICOT Free Software.  The following conditions for ICOT
Free Software applies to the current dictionary as well.

Each User may also freely distribute the Program, whether in its
original form or modified, to any third party or parties, PROVIDED
that the provisions of Section 3 ("NO WARRANTY") will ALWAYS appear
on, or be attached to, the Program, which is distributed substantially
in the same form as set out herein and that such intended
distribution, if actually made, will neither violate or otherwise
contravene any of the laws and regulations of the countries having
jurisdiction over the User or the intended distribution itself.

NO WARRANTY

The program was produced on an experimental basis in the course of the
research and development conducted during the project and is provided
to users as so produced on an experimental basis.  Accordingly, the
program is provided without any warranty whatsoever, whether express,
implied, statutory or otherwise.  The term "warranty" used herein
includes, but is not limited to, any warranty of the quality,
performance, merchantability and fitness for a particular purpose of
the program and the nonexistence of any infringement or violation of
any right of any third party.

Each user of the program will agree and understand, and be deemed to
have agreed and understood, that there is no warranty whatsoever for
the program and, accordingly, the entire risk arising from or
otherwise connected with the program is assumed by the user.

Therefore, neither ICOT, the copyright holder, or any other
organization that participated in or was otherwise related to the
development of the program and their respective officials, directors,
officers and other employees shall be held liable for any and all
damages, including, without limitation, general, special, incidental
and consequential damages, arising out of or otherwise in connection
with the use or inability to use the program or any product, material
or result produced or otherwise obtained by using the program,
regardless of whether they have been advised of, or otherwise had
knowledge of, the possibility of such damages at any time during the
project or thereafter.  Each user will be deemed to have agreed to the
foregoing by his or her commencement of use of the program.  The term
"use" as used herein includes, but is not limited to, the use,
modification, copying and distribution of the program and the
production of secondary products from the program.

In the case where the program, whether in its original form or
modified, was distributed or delivered to or received by a user from
any person, organization or entity other than ICOT, unless it makes or
grants independently of ICOT any specific warranty to the user in
writing, such person, organization or entity, will also be exempted
from and not be held liable to the user for any such damages as noted
above as far as the program is concerned.
```

- https://taku910.github.io/mecab/

## mecab-ipadic-NEologd

Copyright (C) 2015-2019 Toshinori Sato (@overlast)

Licensed under the Apache License, Version 2.0.
NEologd の辞書 (`ipadic-neologd.hsd`, `ipadic-neologd-sudachi.hsd`) の追加語彙データ。
seed のうち `scripts/build-dict.sh` の `NEOLOGD_EXCLUDE` に挙げた 3 つは使わない。

上流に `NOTICE` ファイルは無い (リポジトリの直下にあるのは `COPYING`・README・ChangeLog など)。
`COPYING` は著作権表示、データの出典の表示、Apache License 2.0 の告知からなる。出典は上の
「NEologd (SudachiDict に内包)」に挙げたものと同じで、`COPYING` の文面も mecab-unidic-neologd のものと
URL のほかは同じである。

- https://github.com/neologd/mecab-ipadic-neologd

## 文分割の組み込みの例外表 (src/sentence/builtin_exceptions.txt)

`src/sentence/builtin_exceptions.txt` は、表層に文末記号 (`。！？!?‼⁇⁈⁉．｡`) を含む語
(「モーニング娘。」「Yahoo!ニュース」など) の一覧 (1 行 1 語) で、文分割 (`hasami::sentence`) が
これらの語の内側で文を切らないために使う。表 (と、表から作る照合の索引) はライブラリに埋め込まれ、
文分割と形態素解析 (`Analyzer` は入力を文分割の規則で前分割する) を使うバイナリに入る。hasami の CLI・
Python の拡張モジュール・C FFI のライブラリも含め、辞書ファイルを同梱しなくても入る。

表は推奨辞書 `dict/ipadic-neologd-sudachi.hsd` の全表層形から `hasami export-sentence-exceptions` で
語を選んで整えたもので (抽出規則と生成手順は表の先頭のコメント)、その辞書のソースに由来する。

### 語の出典

表の各語が、辞書のソースのどれに表層形として含まれるかを突き合わせた (1 語が複数のソースに
含まれることがある)。表の語は全角の英数字・記号を半角に畳んであるので、ソースの表層形も同じように
畳んでから比べた。対象は 2026-09-24 時点の表の 18,918 語。表は抽出規則を変えて作り直すことがあるので、
件数は参考値である (畳む前の規則で作った 22,324 語の表では NEologd の語が 22,299 語で、ほかのソースの
語数は同じだった)。

| ソース | 含まれる語 | そのソースにしか無い語 | 例 |
| --- | ---: | ---: | --- |
| mecab-ipadic-NEologd (seed。該当はすべて `mecab-user-dict-seed`) | 18,893 | 18,880 | 「Yahoo!ニュース」「Hey!Say!JUMP」 |
| SudachiDict (raw 辞書 20260723 の small_lex・core_lex) | 29 | 16 | 顔文字「(。A。)」、「Buongiorno!」 |
| mecab-ipadic | 9 | 9 | 「モー娘。」「No．」「ワシントンD．C．」 |
| `dict/user` (hasami 独自の追加語) | 0 | 0 | — |

- どのソースにも無い語は無い。複数のソースに含まれるのは NEologd と SudachiDict の両方にある 13 語だけ。
- SudachiDict の表層形は raw 辞書の Headword (空なら IndexForm。`\u` のエスケープは戻す)。
  `scripts/convert_sudachi_raw.py` は IPAdic・NEologd・`dict/user` に表層形がある語を取り込まないので、
  NEologd と重なる 13 語の辞書のエントリは NEologd のもので、SudachiDict から入ったのは 16 語である。
  内訳は small_lex (`LEGAL` によると UniDic の一部を含む) の 12 語 (顔文字 11 語と「Yonda?」) と、
  core_lex (NEologd の一部を含む) の 4 語。
- IPAdic の CSV は EUC-JP で、hasami はダッシュ・波ダッシュ・マイナスなど 7 字を JIS の対応表の字に写し
  (`src/dict/builder.rs` の `EUC_JP_AMBIGUOUS`)、`scripts/prepare_ipadic.py` が Windows (CP932) 側の字で
  書いた別表記を足す。突き合わせもこれにそろえたが、IPAdic にある表の語はこの 7 字を含まないので差は出ない。

### 同梱すべき表示

表を含むもの (hasami をリンクしたバイナリなど) を配布するときは、次の表示を配布物 (NOTICE、
サードパーティライセンスの一覧、同梱の文書など) に含める。そのまま写せる文面を
[`src/sentence/builtin_exceptions.NOTICE`](src/sentence/builtin_exceptions.NOTICE) に置いた。
表を作り直しても、元になるデータが同じならこの一覧は変わらない。表に掛かるライセンスを SPDX の式で書くと
`NAIST-2003 AND Apache-2.0 AND BSD-3-Clause` である (hasami が語を選んで並べた部分は MIT)。

| データ | ライセンス (SPDX) | 含める表示 | 上流の NOTICE ファイル |
| --- | --- | --- | --- |
| mecab-ipadic | `NAIST-2003` | 著作権表示と、それに続くすべての段落 (上の「mecab-ipadic」の条文の全文) | 無し |
| mecab-ipadic-NEologd | `Apache-2.0` | `COPYING` の全文 (著作権表示・データの出典の表示・ライセンスの告知) と Apache License 2.0 の全文 | 無し |
| SudachiDict | `Apache-2.0` | 著作権表示 (Copyright (c) 2017-2023 Works Applications Co., Ltd.) とライセンスの告知、`LEGAL` の帰属表示、Apache License 2.0 の全文 | `NOTICE` は無い。`LEGAL` を NOTICE とみなす |
| UniDic (SudachiDict の small_lex に含まれる) | `BSD-3-Clause` | 著作権表示 (Copyright (c) 2011-2013, The UniDic Consortium)・条件・免責の全文 (`LEGAL` にある) | 無し |
| NEologd (mecab-unidic-neologd。SudachiDict の core_lex に含まれる) | `Apache-2.0` | `COPYING` の全文 (`LEGAL` にある。mecab-ipadic-NEologd と URL のほかは同じ文面) | 無し |

- Apache License 2.0 の全文は https://www.apache.org/licenses/LICENSE-2.0.txt にある。第 4 条 (a) により、
  Apache-2.0 のデータを含むものには全文の写しを 1 部添える。
- Apache License 2.0 第 4 条 (d) (上流の配布物に `NOTICE` テキストファイルがあれば、その帰属表示を派生物にも含める)
  に当たるファイルは、mecab-ipadic-NEologd と mecab-unidic-neologd には無い。SudachiDict にも `NOTICE` という名前の
  ファイルは無いが、`LEGAL` (見出しは LEGAL NOTICE INFORMATION。上流の履歴でも notice file と呼び、配布物に
  `LICENSE-2.0.txt` と一緒に入れている) を NOTICE とみなし、例外表に関係する表示 (small_lex の UniDic、
  core_lex の NEologd) を含める。`LEGAL` のうち `matrix.def.zip` の項は例外表に関係しない。
- NAIST-2003 と BSD-3-Clause には NOTICE ファイルの仕組みは無いが、条文そのものが表示を求める
  (NAIST-2003 はすべての写しに著作権表示と以下の段落を、BSD-3-Clause はバイナリの再配布で著作権表示・条件・
  免責を文書などに含めることを求める)。
- 表は元のデータから語を選び、表記を整えて表層形以外を除いた派生データである。そのことは表の先頭のコメントと
  NOTICE に書く (Apache License 2.0 第 4 条 (b) の改変の表示)。
- 表を選び出して並べたのは hasami (MIT License) なので、hasami の [LICENSE](LICENSE) も表示する。
  `dict/user` に由来する語が表に入った場合も、その語は hasami の MIT License に従う。

## Unicode Han Database (Unihan) — ビルド時のみ使用

Copyright © 1991-2026 Unicode, Inc.

Licensed under the [Unicode License v3](https://www.unicode.org/license.txt).

外国人名の削除リスト (`dict/user-remove/foreign-names.csv`, `dict/foreign-names/full-names.csv`) を
生成する `scripts/find_foreign_names.py` が、漢字の字音 (kJapanese / kMandarin / kHangul ほか) の参照に使う。
Unihan のデータ自体は辞書にもリポジトリにも含めず、スクリプトの実行時に
`https://www.unicode.org/Public/18.0.0/ucd/Unihan.zip` を取得して SHA-256 を検証する。

- https://www.unicode.org/reports/tr38/
