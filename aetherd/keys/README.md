# aetherd/keys —— 内嵌公钥

本目录只放**公钥**（32 字节原始 Ed25519 公钥，可入库）。私钥绝不入库。

| 文件 | 用途 | 对应私钥 |
|---|---|---|
| `update_pubkey.bin` | 更新包清单验签（`aetherd/src/update.rs`） | owner 离线保管，不在仓库 |
| `app_pubkey.bin` | 应用包清单验签（`aetherd/src/apps.rs`） | 见下方说明 |

## ⚠️ `app_pubkey.bin` 是测试密钥，非生产

- 这把公钥对应的私钥**只用于开发与自测**，由 `scripts/app-keygen.py` 生成，
  默认落在仓库外的 `~/.aether/app-signing-key.pem`，**没有提交进仓库**。
- **绝不能**用这把测试私钥签名发布给用户的应用包。
- 生产发布密钥由 owner **离线保管**（脱机存储，不进任何仓库、镜像、脚本目录）；
  轮换公钥要走发布流程 —— 旧客户端在公钥轮换后将不再接受新签名包。
- 公钥 SHA-256 指纹（生成时记录，供核对）：
  `58f569c93622e31262e2d44ab014a3b1bf12881ab485a21fcf7e4dd208f247b2`

重新生成测试密钥对：

```sh
python scripts/app-keygen.py --selftest   # 自检，不写仓库
python scripts/app-keygen.py              # 重生成（公钥覆盖入库，私钥写仓库外）
```
