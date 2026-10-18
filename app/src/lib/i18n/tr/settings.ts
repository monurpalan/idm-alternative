// Turkish texts for the settings part of the interface, keyed by the English text.
export default {
	'Open magnet links and .torrent files with this app (Windows may ask which app to use)':
		'Magnet linklerini ve .torrent dosyalarını bu uygulamayla aç (Windows hangi uygulamanın kullanılacağını sorabilir)',
	Save: 'Kaydet',
	'Loading…': 'Yükleniyor…',
	Downloads: 'İndirmeler',
	'Save to': 'Kayıt klasörü',
	'Sort into folders by type (Video, Music, Programs…)':
		'Türüne göre klasörlere ayır (Video, Müzik, Programlar…)',
	'Replace files with the same name (otherwise save as “name (1)”)':
		'Aynı adlı dosyaların üzerine yaz (yoksa “ad (1)” olarak kaydet)',
	'Downloads at once': 'Aynı anda indirme',
	'Connections per download': 'İndirme başına bağlantı',
	'Speed limit': 'Hız sınırı',
	none: 'yok',
	'The speed limit must be a positive number of KB/s, or empty.':
		'Hız sınırı pozitif bir KB/s değeri olmalı ya da boş bırakılmalı.',
	Connection: 'Bağlantı',
	'Plain client': 'Sade istemci',
	Proxy: 'Ara sunucu (proxy)',
	'none, or socks5://host:port': 'yok ya da socks5://sunucu:port',
	'Start and stop the queue at set times': 'Kuyruğu belirli saatlerde başlat ve durdur',
	'Start at': 'Başlangıç',
	'Stop at': 'Bitiş',
	Days: 'Günler',
	Mon: 'Pzt',
	Tue: 'Sal',
	Wed: 'Çar',
	Thu: 'Per',
	Fri: 'Cum',
	Sat: 'Cmt',
	Sun: 'Paz',
	Forget: 'Unut',
	'Video tools': 'Video araçları',
	Found: 'Bulundu',
	'Not found: install it (e.g. winget install ffmpeg) to join video and audio':
		'Bulunamadı: görüntü ile sesi birleştirmek için kurun (ör. winget install ffmpeg)',
	'YouTube, X, Instagram and other sites': 'YouTube, X, Instagram ve diğer siteler',
	'Needed by yt-dlp for YouTube': 'yt-dlp, YouTube için buna ihtiyaç duyar',
	'Version {v}': 'Sürüm {v}',
	Update: 'Güncelle',
	'Set up': 'Kur',
	'Update yt-dlp and deno when a new version is out (checked once a day)':
		'Yeni sürüm çıktığında yt-dlp ve deno’yu güncelle (günde bir kez denetlenir)',
	"Downloaded from the projects' official GitHub releases and checked against their published SHA-256.":
		'Projelerin resmî GitHub sürümlerinden indirilir ve yayımlanan SHA-256 değerleriyle doğrulanır.',
	App: 'Uygulama',
	Language: 'Dil',
	'Same as Windows': 'Windows ile aynı',
	'Keep running in the tray when the window is closed':
		'Pencere kapatılınca sistem tepsisinde çalışmaya devam et',
	'Start with Windows (in the tray)': 'Windows ile başlat (sistem tepsisinde)',
	'Show a small progress window for each download':
		'Her indirme için küçük bir ilerleme penceresi göster',
	'Notify me when a download finishes': 'Bir indirme bitince bildir',
	'Check now': 'Şimdi denetle',
	'Checking…': 'Denetleniyor…',
	'This is the latest version.': 'En son sürümü kullanıyorsunuz.',
	'Could not check: {e}': 'Denetlenemedi: {e}',
	'Scan finished downloads for viruses': 'Biten indirmeleri virüslere karşı tara',
	Scanner: 'Tarayıcı',
	'Microsoft Defender, or: "C:\\…\\scan.exe" {f}':
		'Microsoft Defender ya da: "C:\\…\\scan.exe" {f}',
	'Offer download links I copy (archives, programs, video, music, documents)':
		'Kopyaladığım indirme bağlantılarını öner (arşivler, programlar, video, müzik, belgeler)',
	'Export settings…': 'Ayarları dışa aktar…',
	'Import settings…': 'Ayarları içe aktar…',
	'Open the log folder': 'Kayıt klasörünü aç',
	'Settings saved to {path}': 'Ayarlar kaydedildi: {path}',
	'Could not save the settings: {e}': 'Ayarlar kaydedilemedi: {e}',
	'Settings loaded and applied': 'Ayarlar yüklendi ve uygulandı',
	'Could not load the settings: {e}': 'Ayarlar yüklenemedi: {e}',
	Schedule: 'Zamanlama',
	'Sign-ins': 'Girişler',
	'Browser and system': 'Tarayıcı ve sistem',
	'Version {v} is ready: install it from the bar at the bottom of the window.':
		'Sürüm {v} hazır: pencerenin altındaki çubuktan kurabilirsiniz.',
	'Speed limit for all (KB/s)': 'Toplam hız sınırı (KB/s)',
	'Introduce itself as': 'Kendini tanıtacağı tarayıcı',
	'Some servers only serve browsers: the app then sends the same headers as the one chosen.':
		'Bazı sunucular yalnızca tarayıcılara yanıt verir: uygulama seçilen tarayıcının başlıklarını gönderir.',
	'Leave a time empty to only start or only stop. To shut the computer down after the queue, use “When all finish” in the side column.':
		'Yalnızca başlatmak ya da yalnızca durdurmak için saatlerden birini boş bırakın. Kuyruk bitince bilgisayarı kapatmak için yan sütundaki “Hepsi bitince” seçeneğini kullanın.',
	'None yet. Tick “Remember” when a server asks for a password.':
		'Henüz yok. Bir sunucu parola istediğinde “Hatırla”yı işaretleyin.',
	'Take downloads from the browser extension (Chrome, Edge, Brave, Vivaldi, Opera and Firefox)':
		'Tarayıcı eklentisinden gelen indirmeleri al (Chrome, Edge, Brave, Vivaldi, Opera ve Firefox)',
	'Look for a new version once a day': 'Yeni sürümü günde bir kez denetle'
} satisfies Record<string, string>;
