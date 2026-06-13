# Changelog

## 3.0.0 - 2026-06-13

- Sürümlenmiş `.deepgrep-v3` base ve delta indeks biçimlerini ekledi.
- Regex AST'sinden güvenli zorunlu literal çıkarımı ve indeksli regex planı ekledi.
- `--explain`, glob/tür filtreleri, JSON, dosya listesi ve sayım modları ekledi.
- `--hidden`, `--no-ignore` ve `--text` arama seçeneklerini ekledi.
- Büyük dosyaları akış halinde indeksleyerek boyut kaynaklı sonuç kaybını giderdi.
- Binary dosyaları indeksli aramada güvenli doğrulama adayı olarak sakladı.
- İndeks izolasyonu, filtreler, çıktı modları, binary dosyalar ve indeksli regex için
  regresyon testleri ekledi.
