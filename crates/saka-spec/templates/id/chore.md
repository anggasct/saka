---
saka: spec
kind: chore
# depends_on: [spec-lain]   spec yang harus selesai sebelum spec ini mulai
# priority: 0               -1000..1000; yang lebih besar mulai lebih dulu
---
# Judul singkat chore

Apa yang perlu dikerjakan dan alasannya, misalnya menaikkan versi dependensi, mengubah konfigurasi, atau merapikan.

## Acceptance criteria

- **AC-01**: satu hasil yang bisa dicek dengan perintah.

## Done when

- `make test`

## Notes

- Chore tidak mengubah apa pun yang terlihat pengguna. Pakai kind `feature` kalau perilaku berubah dan `bugfix` kalau memperbaiki bug.
- Semua perintah di list `Done when` harus lulus sebelum step selesai.
