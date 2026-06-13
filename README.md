# Deepgrep v3 (`dg`)

[![CI](https://github.com/EmirHKL/Deepgrep-v3/actions/workflows/ci.yml/badge.svg)](https://github.com/EmirHKL/Deepgrep-v3/actions/workflows/ci.yml)

Deepgrep v3, tekrar eden kod aramalarında `ripgrep`'in ham tarama motorunu kalıcı
bir trigram indeks, güvenli regex planlayıcısı ve artımlı güncelleme sistemiyle
geliştiren Rust tabanlı bir arama aracıdır.

V3 bağımsız bir projedir. Kararlı v2.1 sürümü
[`EmirHKL/Deepgrep`](https://github.com/EmirHKL/Deepgrep) deposunda korunur.
V3, ayrı `.deepgrep-v3` indeks biçimini kullanır ve v2.1 indeksine dokunmaz.

## V3 Neyi Farklı Yapıyor?

- Düz metin sorgularını binary trigram indeksle hızlandırır.
- Regex AST'sinden her eşleşmede bulunması zorunlu literal parçayı güvenle çıkarır.
- İndekslenebilir regex sorgularında önce aday dosyaları azaltır, sonra tam regex
  doğrulaması yapar; sonuç kaçırmaz.
- `--explain` ile seçilen sorgu planını ve indeks ön filtresini gösterir.
- `dg watch .` ile değişen dosyaları kalıcı delta indeksine işler.
- `-g`, `-t`, `-T`, `--hidden` ve `--no-ignore` filtrelerini destekler.
- `--json`, `-l/--files-with-matches` ve `-c/--count` çıktı modlarını destekler.
- `-a/--text` ile binary dosyaları metin olarak arayabilir.
- Büyük dosyaları belleğe tamamen almadan akış halinde indeksler.
- Binary dosyaları indeksli aramada güvenli doğrulama adayı olarak saklar.

Deepgrep, eşleşme doğrulamasında ripgrep ekosistemindeki `grep-regex`,
`grep-searcher`, `grep-matcher` ve `ignore` crate'lerini kullanır. Özgün katkısı;
bunların önündeki kalıcı indeks, regex planlayıcısı, artımlı güncelleme ve
açıklanabilir sorgu planıdır.

## Performans

13 Haziran 2026 tarihinde şu ortamda ölçüldü:

- CPU: Intel Core i7-11800H, 8 çekirdek / 16 thread
- Corpus: yerel Cargo registry, 6.374 dosya
- İndeks: 343.378 trigram, 46,0 MiB, 1,10 saniye
- Araç: `hyperfine`, 3 ısınma ve 5 ölçüm
- Her senaryoda Deepgrep ve ripgrep sonuç sayıları önce karşılaştırıldı

| Senaryo | Deepgrep v3 | ripgrep | Sonuç |
|---|---:|---:|---:|
| İndeksli nadir literal | 24,8 ms | 101,5 ms | Deepgrep **4,09x hızlı** |
| İndeksli yaygın literal (`unsafe`) | 45,1 ms | 104,6 ms | Deepgrep **2,32x hızlı** |
| İndeksli regex (`S[a-z]+MatcherV2`) | 29,8 ms | 117,5 ms | Deepgrep **3,94x hızlı** |
| İndekssiz nadir literal | 94,0 ms | 102,2 ms | Deepgrep **1,09x hızlı** |
| İndekssiz regex | 105,3 ms | 108,7 ms | Aynı performans bandı |

Bu sonuçların dürüst sınırı şudur: hız üstünlüğü tekrar eden, indekslenebilir
aramalarda belirgindir. İndeks oluşturmanın disk alanı ve başlangıç maliyeti
vardır. Ham tarama performansı corpus ve sorguya göre iki araç arasında değişebilir.

## Kurulum

```powershell
git clone https://github.com/EmirHKL/Deepgrep-v3
cd Deepgrep-v3
cargo build --release
```

Binary:

```text
target/release/dg.exe
```

## Kullanım

```powershell
# Projeyi bir kez indeksle
dg index .

# İndeksli literal ve regex aramaları
dg SearchOptions .
dg "fn\s+SearchOptions" . --explain

# Filtreleme
dg unsafe . -t rust -g "!tests/**"
dg token . --hidden
dg token . --no-ignore

# Yapılandırılmış veya özet çıktı
dg SearchOptions . --json
dg SearchOptions . --files-with-matches
dg SearchOptions . --count

# Binary dosyaları metin kabul et
dg token . --text

# İndeksi artımlı güncel tut
dg watch .

# İndekssiz ham tarama
dg SearchOptions . --no-index

# Yalnızca v3 indeksini temizle
dg clean .
```

`--explain` örneği:

```text
plan: regex mandatory-literal index + ripgrep regex verification
index prefilter: "SearchOptions"
```

## Doğruluk ve Güvenilirlik

- Regex ön filtresi yalnızca her olası eşleşmede zorunlu olduğu kanıtlanan
  literal parçaları kullanır.
- Aday dosyalar tam regex motoruyla yeniden doğrulanır.
- İndeksli ve ham arama çıktıları entegrasyon testlerinde karşılaştırılır.
- 32 MiB üzerindeki dosyalar akış halinde indekslenir; boyut nedeniyle atlanmaz.
- Binary dosyalar indeksli sorgularda sonuç kaybına yol açmaz.
- V2 ve v3 indekslerinin birbirine dokunmadığı test edilir.
- İndeks geçici dosyaya yazılır ve tamamlandığında atomik olarak değiştirilir.
- Çıkış kodları grep uyumludur: eşleşme `0`, eşleşme yok `1`, hata `2`.

Toplam 54 birim ve entegrasyon testi; Windows, Linux ve macOS GitHub Actions
matrisinde çalışır.

## Mimari

```text
query
  |
  +-- güvenli literal veya zorunlu regex literali
  |     |
  |     +-- mmap binary trigram indeksini sorgula
  |     +-- en seyrek posting listelerinden başla
  |     +-- base ve delta adaylarını birleştir
  |     +-- adayları ripgrep regex motoruyla doğrula
  |
  +-- güvenli indeks ön filtresi yok
        |
        +-- ignore-aware paralel dizin taraması
        +-- ripgrep regex motoruyla tam tarama
```

Ayrıntılar: [`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)

Değerlendirme rehberi: [`docs/EVALUATION.md`](docs/EVALUATION.md)

## Doğrulama

```powershell
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo build --release
.\benchmarks\compare.ps1
```

## Ripgrep'e Göre Sınırlar

Deepgrep v3 her ripgrep seçeneğini kopyalamayı hedeflemez. PCRE2, çok satırlı
arama, bağlam satırları ve encoding seçimi henüz yoktur. Buna karşılık ripgrep'te
bulunmayan kalıcı trigram indeks, artımlı watcher, indeksli regex hızlandırma ve
açıklanabilir sorgu planı sunar.

## Lisans

MIT
