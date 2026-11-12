---
saka: spec
kind: refactor
# depends_on: [spec-lain]   spec yang harus selesai sebelum spec ini mulai
# priority: 0               -1000..1000; yang lebih besar mulai lebih dulu
---
# Judul singkat refactor

Apa yang berubah pada struktur kode dan alasannya. Perilaku tetap sama.
Sebutkan file atau modul yang terlibat kalau sudah tahu.

## Acceptance criteria

- **AC-01**: perilaku tidak berubah: test yang ada lulus tanpa mengubah ekspektasinya.

## Done when

- `make test`

## Notes

- Refactor mengubah struktur, bukan perilaku. Kalau sebuah test harus berubah, jelaskan alasannya di deskripsi.
- Semua perintah di list `Done when` harus lulus sebelum step selesai.
