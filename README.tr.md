# linkedin-unfollower

LinkedIn'de takip ettiğin şirketleri listeler ve toplu olarak takipten
çıkarır — bilinçli olarak yavaş, rastgele aralıklı ve kaldığı yerden devam
edebilen bir hızda.

İki parça, her biri tek başına da işe yarar:

| | Nedir | Nerede çalışır | Ne zaman |
|---|---|---|---|
| **`web/unfollow.js`** | ~500 satır okunabilir, sade JavaScript | DevTools console'a yapıştırılır | **Buradan başla.** Takip ettiklerini listeler, dışa aktarır, gerçek arayüzü kullanarak takipten çıkarır. |
| **`cli/` (`liunfollow`)** | Rust CLI | terminalde | Yüzlerce şirket, başında beklemeden, saatler boyunca. *Senin* yakaladığın isteği tekrar oynatır. |

## Console kısmı neden Rust değil

O kod, LinkedIn'e giriş yapmış oturumunun içinde çalışıyor. Rust→WebAssembly
paketi, kimsenin — senin de — kendi oturum çerezinin yanına yapıştırmadan önce
denetleyemeyeceği base64 bir blob olarak gelirdi. Bu yüzden o parça bilinçli
olarak okunabilir JavaScript kaldı. Gerçek mühendislik Rust tarafında:
hız ayarı, geri çekilme (backoff), yanıt sınıflandırma, ilerleme kaydı.

## LinkedIn API'si neden hardcode edilmedi

LinkedIn'in dahili (Voyager) endpoint'leri, GraphQL query id'leri ve zorunlu
header'ları haber vermeden değişir. Bunları sabitleyen her araç birkaç ay içinde
bozulur. Bu araç hiçbirini sabitlemiyor:

* **console aracı** butonları LinkedIn'in şifrelenmiş CSS sınıflarıyla değil,
  erişilebilirlik etiketiyle bulur (Türkçe ve İngilizce arayüzde çalışır);
* **CLI**, kendi Network sekmenden kopyaladığın isteği tekrar oynatır.

---

## Hızlı başlangıç — console aracı

1. <https://www.linkedin.com/mynetwork/network-manager/company/> adresini aç
2. DevTools → Console. Chrome sorarsa önce `allow pasting` yaz.
3. [`web/unfollow.js`](web/unfollow.js) dosyasının tamamını yapıştır, Enter.
4. Sonra:

```js
await LIU.scan()                        // listeyi sonuna kadar yükler
LIU.list()                              // tablo olarak yazdırır
LIU.export()                            // JSON çıktısı (Rust CLI bunu yer)
await LIU.unfollow()                    // PROVA — hiçbir şeye tıklamaz
await LIU.unfollow({ confirm: true })   // gerçek, çalıştırma başına en fazla 50
```

`{ confirm: true }` vermediğin sürece hiçbir şeye tıklanmaz.

| Komut | Ne yapar |
|---|---|
| `LIU.unfollow({ confirm: true, limit: 10 })` | sadece ilk 10'u |
| `LIU.unfollow({ only: /recruit/i, confirm: true })` | isim/slug regex'e uyanlar |
| `LIU.unfollow({ except: ['Acme Corp', 'is-yerim'], confirm: true })` | bunlar kalsın |
| `LIU.stop()` | mevcut adımdan sonra durdurur |
| `LIU.config({ minDelay: 3000 })` | daha da yavaşlatır |
| `LIU.resetState()` | yapılanları unutur |

İlerleme `localStorage`'da tutulur; aynı komutu yeniden çalıştırınca kaldığı
yerden devam eder. Sekmeyi kapatıp yarın dönebilirsin.

### Varsayılan hız

* tıklamalar arası 1.8–4.2 sn, rastgele
* her 15 işlemde bir 20–40 sn mola
* çalıştırma başına katı sınır: 50
* üst üste 3. hatada durur
* `fetch`/XHR durum kodlarını izler ve **429/999/403 görür görmez durur**

Yaklaşık 5 dakikada 50 şirket. 400 şirketi tek oturumda takipten çıkarmak, seni
doğrudan rate-limit sayfasına götürür — birkaç kez çalıştır, daha iyi.

---

## Hızlı başlangıç — Rust CLI

```bash
cd cli && cargo build --release    # ikili dosya: cli/target/release/liunfollow
```

CLI, takipten çıkarmayı kendiliğinden bilmiyor. Bir kez sen öğretiyorsun:

1. Şirketler sayfasında DevTools → **Network** sekmesini aç.
2. **Bir** şirketi elle takipten çıkar.
3. Bunu yapan isteği bul (bir POST; `follow` ile filtrele), sağ tık →
   **Copy** → **Copy as cURL**.
4. `unfollow.curl` olarak kaydet ve `chmod 600 unfollow.curl` çalıştır —
   **içinde oturum çerezin var.**
5. Dosyadaki tek şirket kimliğini `{urn}` yer tutucusuyla değiştir:

```diff
- --data-raw '{"urn":"urn:li:fsd_company:1234567"}'
+ --data-raw '{"urn":"{urn}"}'
```

Kullanılabilir yer tutucular: `{urn}`, `{urn_enc}` (URL yolu için
yüzde-kodlanmış), `{id}`, `{slug}`, `{name}`. URL'de, gövdede ve her header'da
çalışırlar.

Sonra:

```bash
liunfollow doctor --request unfollow.curl
```

```bash
liunfollow plan --list companies.json --request unfollow.curl
```

```bash
liunfollow run --list companies.json --request unfollow.curl --max 25 --yes
```

`companies.json`, `LIU.export()` çıktısı. **`--yes` vermediğin her çalıştırma
provadır.**

```bash
liunfollow status
```

Adım adım anlatım ve sorun giderme: [docs/capture.md](docs/capture.md).

### CLI senin için ne yapıyor

* **Kendi hızını ayarlar** — istekler arası 4 sn ±%40 rastgelelik, her 15'te
  30 sn mola.
* **Geri çekilir** — 429 / 999 / 5xx gelince 60 sn bekler, sonra 120, 240 …
  15 dakikada tavan yapar, `--retries` kadar denemeyle.
* **Ne zaman duracağını bilir** — 401/403 ya da yönlendirme, yakaladığın
  isteğin süresinin dolduğu anlamına gelir; listeyi tüketmek yerine anında
  durur. Üst üste üç hata da durdurur.
* **Kaldığı yerden devam eder** — kayıt dosyası **her şirketten sonra**
  yeniden yazılır. Ctrl-C sana en fazla bir isteğe mal olur.
* **Çerezini asla yazdırmaz** — `cookie`, `authorization` ve `csrf-token`
  değerleri tüm çıktıda maskelenir; yakalama dosyası başka kullanıcılar
  tarafından okunabiliyorsa uyarır.

### Seçenekler

```
--only <REGEX>        sadece isim/slug/urn eşleşen şirketler
--except <REGEX>      eşleşen şirketleri atla
--max <N>             tek çalıştırmada en fazla N şirket          [50]
--interval <SANIYE>   istekler arası temel aralık                 [4.0]  (en az 0.5)
--jitter <0..1>       her aralığı bu oranda rastgeleleştir        [0.4]
--batch-size <N>      her N istekte uzun mola, 0 = kapalı         [15]
--batch-pause <SN>    bu molanın süresi                           [30]
--retries <N>         rate limit'te şirket başına yeniden deneme  [3]
--abort-after <N>     üst üste N hatadan sonra vazgeç             [3]
--backoff <SANIYE>    ilk geri çekilme, her denemede ikiye katlar [60]   (en az 1)
--backoff-max <SN>    bu katlanmanın tavanı                       [900]
--retry-failed        daha önce başarısız olanları tekrar dene
--state <DOSYA>       kayıt dosyası        [liunfollow-state.json]
--yes                 istekleri gerçekten gönder
```

---

## Hangisini kullanmalıyım?

Aksini gerektiren bir durum yoksa **console aracı**. Derleme adımı yok, diskte
çerez yok, senin tıklayacağın butonlara tıklıyor. CLI, console aracının kötü
olduğu durum için var: birkaç yüz şirket, başında beklemeden, tarayıcın serbest
kalsın diye.

## Güvenlik notları

* LinkedIn Kullanıcı Sözleşmesi otomatik erişimi kısıtlar. Console aracı
  yalnızca senin de tıklayabileceğin butonlara, senin dayanabileceğinden daha
  yavaş bir hızda tıklıyor; CLI ise senin kendi yaptığın isteği tekrarlıyor. Bu
  yine de otomasyondur ve risk sana ait.
* Yakaladığın cURL dosyası bir kimlik bilgisidir. `chmod 600` yap, asla commit
  etme — `.gitignore` zaten `*.curl` dosyalarını engelliyor, `*.curl.example`
  hariç.
* Bu depo, bilinçli olarak **hiçbir** çalışan LinkedIn endpoint'i, query id'si
  veya token'ı içermiyor.

## Geliştirme

```bash
make test
```

```bash
make lint
```

* Rust: cURL ayrıştırma, hız ayarı, backoff, planlama, HTTP sınıflandırma,
  yeniden deneme ve kayıt dosyası üzerinde 50 birim testi — ağ gerekmiyor.
* JavaScript: elle yazılmış sahte bir DOM üzerinde 8 test; keşif, dışa aktarma
  şeması, prova, devam etme, filtreler, hata durdurması ve 429 gözcüsü.

## Lisans

MIT — [LICENSE](LICENSE). Yayınlamadan önce telif satırına kendi adını yaz.
