---
saka: spec
kind: bugfix
# depends_on: [spec-lain]   spec yang harus selesai sebelum spec ini mulai
# priority: 0               -1000..1000; yang lebih besar mulai lebih dulu
---
# Judul singkat perbaikan bug

Apa yang salah, langkah untuk memunculkannya, dan apa yang seharusnya terjadi.
Sebutkan file atau modul yang terlibat kalau sudah tahu.

## Acceptance criteria

- **AC-01**: bug tidak terjadi lagi, dibuktikan oleh test yang gagal sebelum perbaikan.

## Regression

- `make test-regression`

## Done when

- `make test`

## Notes

- Test dulu: agent menulis test yang memunculkan bug, hanya mengubah file test, dan meng-commit-nya sebelum menyentuh perbaikan. Saka menjalankan setiap perintah `## Regression` pada commit itu dan mewajibkan minimal satu gagal. Baru setelah itu agent memperbaiki bug.
- Setiap item `## Regression` berisi tepat satu perintah dalam backtick, paling banyak 10. Arahkan ke test yang baru, bukan ke seluruh suite.
- Semua perintah di `## Regression` dan `## Done when` harus lulus pada perubahan akhir.
- File test dikenali dari path (`tests/`, `*_test.*`, `*.spec.*`, dan sejenisnya). Repo dengan susunan lain menulis polanya sebagai `test_paths` untuk kind ini di `saka.toml`.
- Aturan test dulu hanya berlaku untuk step pertama, jadi buat perbaikan bug satu step.
