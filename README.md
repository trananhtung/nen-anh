# Nén Ảnh

Phần mềm Windows giảm dung lượng ảnh JPG, PNG, WebP mà giữ nguyên độ phân giải. Ảnh được xử lý ngay trên máy, không tải lên mạng.

## Cài đặt

Vào mục **Releases**, tải một trong hai file:

- `NenAnh-x.y.z-setup.exe`: bản nhẹ (khoảng 5 MB), dùng cho hầu hết máy Windows 10/11.
- `NenAnh-x.y.z-setup-offline.exe`: kèm sẵn WebView2 (khoảng 300 MB), dùng khi máy không có mạng.

Bộ cài chưa có chữ ký số nên Windows SmartScreen có thể cảnh báo. Bấm **More info** rồi **Run anyway** nếu bạn tải từ trang này. Có thể đối chiếu file với `SHA256SUMS.txt`.

## Cách dùng

1. Kéo ảnh hoặc thư mục vào cửa sổ.
2. Chọn định dạng và mức nén.
3. Bấm **Nén**. Kết quả nằm trong thư mục `anh-nen` cạnh ảnh gốc, hoặc nơi bạn chọn.

| Mức nén | Cam kết |
|---|---|
| Không mất dữ liệu | Giống bản gốc từng điểm ảnh, có giải mã lại để kiểm tra |
| Chất lượng cao | Điểm SSIMULACRA2 tối thiểu 90 |
| Cân bằng | Tối thiểu 85 |
| Nhẹ nhất | Tối thiểu 75 |

- Không bao giờ ghi đè file có sẵn. Ảnh không giảm được thì chép nguyên bản gốc.
- Ảnh động, ảnh HDR và ảnh kèm video được giữ nguyên. Ảnh CMYK và PNG 16-bit chỉ nén không mất dữ liệu.
- Đổi sang WebP/AVIF sẽ xoay ảnh đúng chiều theo thông tin EXIF.
- Với AVIF, điểm được đo trên ảnh dựng lại của bộ mã hoá và trừ đi 3 điểm dự phòng.
- JPG nhúng ảnh xem trước (MPF) sẽ bỏ phần xem trước đó.

## Công nghệ

Rust (edition 2024), Tauri 2.12, React 19.3, TypeScript 7, Vite 8. Bộ nén: mozjpeg, oxipng, quantette, libwebp, rav1e. Xem [THIRD-PARTY.md](THIRD-PARTY.md).

## Phát triển

```bash
npm ci
npm run tauri dev
cargo test -p nenanh-core
```

Đẩy tag `vX.Y.Z` để GitHub Actions build, cài thử trên Windows và phát hành.
