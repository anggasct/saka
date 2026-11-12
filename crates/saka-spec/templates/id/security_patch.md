---
saka: spec
kind: security_patch
# depends_on: [spec-lain]   spec yang harus selesai sebelum spec ini mulai
# priority: 0               -1000..1000; yang lebih besar mulai lebih dulu
---
# Judul singkat perbaikan keamanan

Kelemahannya, letaknya, dan siapa yang terdampak. Jangan tulis detail eksploitasi di deskripsi kalau repo-nya publik.
Sebutkan file atau modul yang terlibat kalau sudah tahu.

## Acceptance criteria

- **AC-01**: kelemahan tertutup, dibuktikan oleh pemeriksaan yang gagal tanpa patch.

## Done when

- `make test`

## Notes

- Saka selalu me-review patch keamanan dan meminta persetujuan operator untuk setiap step dan merge, apa pun isi workflow project.
- Semua perintah di list `Done when` harus lulus sebelum step selesai.
