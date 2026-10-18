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
	Dismiss: 'Kapat',

	// Right-click menu
	Open: 'Aç',
	'Show in folder': 'Klasörde göster',
	'Show progress window': 'İlerleme penceresini göster',
	'Move to top': 'En üste taşı',
	'Move to bottom': 'En alta taşı',
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
	'Verifying (checksum from {from}) {v}%': 'Doğrulanıyor ({from} ile) %{v}',
	Downloading: 'İndiriliyor',
	Connecting: 'Bağlanıyor',
	Retrying: 'Yeniden deneniyor',
	Waiting: 'Bekliyor',
	Paused: 'Duraklatıldı',
	'Login needed': 'Giriş gerekli',
	'Link expired': 'Bağlantı eskidi',
	'No downloads here': 'Burada indirme yok',
	'Paste a link with Ctrl+V, drop one on the window, or choose New download.':
		'Ctrl+V ile bir bağlantı yapıştırın, pencereye bırakın ya da Yeni indirme’yi seçin.'
} satisfies Record<string, string>;
