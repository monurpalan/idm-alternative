// Turkish texts for the main window, keyed by the English text.
export default {
	// Side column
	'Grab files from a site': 'Siteden dosya topla',
	Lists: 'Listeler',
	Status: 'Durum',
	Type: 'Tür',
	'{n} running': '{n} çalışıyor',
	'limit {speed}': 'sınır {speed}',
	'When all finish': 'Hepsi bitince',
	'Do nothing': 'Hiçbir şey yapma',
	'Close the app': 'Uygulamayı kapat',
	Sleep: 'Uyku',
	Hibernate: 'Hazırda beklet',
	'Shut down': 'Bilgisayarı kapat',
	Settings: 'Ayarlar',

	// Top bar
	'{n} selected': '{n} seçili',
	Resume: 'Sürdür',
	Pause: 'Duraklat',
	'Clear the selection': 'Seçimi kaldır',
	'Resume all': 'Tümünü sürdür',
	'Pause all': 'Tümünü duraklat',
	Search: 'Ara',

	// Messages under the list
	'Version {version} is ready.': 'Sürüm {version} hazır.',
	'Running downloads continue after the restart.':
		'Süren indirmeler yeniden başlayınca kaldığı yerden sürer.',
	'Restart and update': 'Yeniden başlat ve güncelle',
	'Downloading…': 'İndiriliyor…',
	Later: 'Sonra',
	'The extension in {browser} is out of date.': '{browser} içindeki eklenti eski.',
	'Update it so downloads keep reaching the app.':
		'İndirmelerin uygulamaya ulaşmaya devam etmesi için eklentiyi güncelleyin.',
	'Browser extension not found.': 'Tarayıcı eklentisi bulunamadı.',
	'Install it to download from the browser with this app.':
		'Tarayıcıdan bu uygulamayla indirmek için eklentiyi kurun.',
	Install: 'Kur',
	'Don’t show again': 'Bir daha gösterme',
	Dismiss: 'Kapat',

	// Right-click menu
	Open: 'Aç',
	'Show in folder': 'Klasörde göster',
	'Show progress window': 'İlerleme penceresini göster',
	'Move to top': 'En üste taşı',
	'Move to bottom': 'En alta taşı',
	'Rename…': 'Yeniden adlandır…',
	'Move…': 'Taşı…',
	'Copy link': 'Bağlantıyı kopyala',
	'Download again': 'Yeniden indir',
	'Remove…': 'Kaldır…',

	// Download list
	Name: 'Ad',
	Size: 'Boyut',
	Progress: 'İlerleme',
	Speed: 'Hız',
	'Time left': 'Kalan',
	Added: 'Eklendi',
	'Verifying {v}%': 'Doğrulanıyor %{v}',
	'Setting up ffmpeg {v}%': 'ffmpeg kuruluyor %{v}',
	'Verifying (checksum from {from}) {v}%': 'Doğrulanıyor ({from} ile) %{v}',
	Downloading: 'İndiriliyor',
	Connecting: 'Bağlanıyor',
	Retrying: 'Yeniden deneniyor',
	Waiting: 'Bekliyor',
	'Waiting for the network': 'Ağ bekleniyor',
	Paused: 'Duraklatıldı',
	'Moving {p}%': 'Taşınıyor %{p}',
	'Login needed': 'Giriş gerekli',
	'Link expired': 'Bağlantı eskidi',
	'No downloads here': 'Burada indirme yok',
	'Paste a link with Ctrl+V, drop one on the window, or choose New download.':
		'Ctrl+V ile bir bağlantı yapıştırın, pencereye bırakın ya da Yeni indirme’yi seçin.',
	'Fold the group': 'Grubu katla',
	'Show the group': 'Grubu aç',
	'{done} of {n} done': '{done}/{n} bitti',
	'Rec. {time}': 'Kayıt {time}',
	'Starts at {time}': '{time} başlar'
} satisfies Record<string, string>;
