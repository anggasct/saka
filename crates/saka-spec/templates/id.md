---
saka: spec
# depends_on: [spec-lain]   spec yang harus selesai sebelum spec ini mulai
# priority: 0               -1000..1000; yang lebih besar mulai lebih dulu
---
# Judul singkat perubahan

Jelaskan masalahnya, untuk siapa, dan apa yang berubah setelah ini selesai.
Sebutkan file atau modul yang terlibat kalau sudah tahu.

## Acceptance criteria

- **AC-01**: satu hasil yang bisa dicek dengan perintah.
- **AC-02**: hasil lain, misalnya balasan API atau test yang lulus.

## Steps

| Step | Work | AC | Done when |
|------|------|----|-----------|
| S1 | Bagian kerja pertama | AC-01 | `make test` |
| S2 | Bagian kerja kedua, di atas S1 | AC-02 | `make test` `make lint` |

## Notes

- Step berjalan berurutan. Supaya step berjalan paralel, tambahkan kolom `After` dan tulis step yang ditunggu, misalnya `S1` atau `S1, S2`. Sel kosong berarti step itu tidak menunggu apa pun.
- Untuk perubahan kecil, ganti bagian Steps dengan bagian `Done when`: list berisi satu perintah dalam backtick per item.
- Semua perintah di kolom `Done when` harus lulus sebelum sebuah step selesai.
