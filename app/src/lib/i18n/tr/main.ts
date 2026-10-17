// Turkish texts for the main part of the interface, keyed by the English text.
export default {
	// Toolbar
	'Add URL': 'URL ekle',
	Resume: 'Devam',
	Pause: 'Duraklat',
	'Start all': 'Tümünü başlat',
	'Queue every paused or failed download': 'Duraklatılan ve hata veren tüm indirmeleri sıraya al',
	'Pause all': 'Tümünü duraklat',
	'Pause every running and queued download': 'Süren ve sıradaki tüm indirmeleri duraklat',
	'When done': 'Bitince',
	'One time: when nothing is downloading or waiting any more':
		'Tek seferlik: indirilen ya da bekleyen bir şey kalmadığında',
	'do nothing': 'hiçbir şey yapma',
	'close the app': 'uygulamayı kapat',
	sleep: 'uyku',
	hibernate: 'hazırda beklet',
	'shut down': 'kapat',
	Details: 'Ayrıntılar',
	'Details of the selected download': 'Seçili indirmenin ayrıntıları',
	Settings: 'Ayarlar',
	Search: 'Ara',

	// Status bar
	'idm-alternative {version} is available.': 'idm-alternative {version} sürümü çıktı.',
	'Downloading…': 'İndiriliyor…',
	'Install and restart': 'Yükle ve yeniden başlat',
	'Running downloads pause and continue after the restart.':
		'Süren indirmeler duraklar, yeniden başlatmadan sonra devam eder.',
	Later: 'Sonra',
	Dismiss: 'Kapat',
	'{n} downloading': '{n} indiriliyor',
	'Speed limit for all downloads': 'Tüm indirmeler için hız sınırı',
	'limit {speed}': 'sınır {speed}',
	'1 item': '1 öğe',
	'{n} items': '{n} öğe',
	'{n} selected': '{n} seçili',

	// Right-click menu
	Open: 'Aç',
	'Show in folder': 'Klasörde göster',
	'Progress window': 'İlerleme penceresi',
	'Move to top': 'En üste taşı',
	'Move up': 'Yukarı taşı',
	'Move down': 'Aşağı taşı',
	'Move to bottom': 'En alta taşı',
	'Copy address': 'Adresi kopyala',
	'Download again': 'Yeniden indir',
	'Remove…': 'Kaldır…',

	// Download list
	Name: 'Ad',
	Size: 'Boyut',
	Progress: 'İlerleme',
	Speed: 'Hız',
	'Time left': 'Kalan süre',
	Status: 'Durum',
	Added: 'Eklenme',
	'Verifying {v}%': 'Doğrulanıyor %{v}',
	Downloading: 'İndiriliyor',
	Connecting: 'Bağlanıyor',
	Retrying: 'Yeniden deneniyor',
	Queued: 'Sırada',
	Paused: 'Duraklatıldı',
	Complete: 'Tamamlandı',
	'Login needed': 'Giriş gerekli',
	'Link expired': 'Süresi doldu',
	Error: 'Hata',
	'Nothing here. Paste a link (Ctrl+V) or drop one on the window.':
		'Burada bir şey yok. Bir bağlantı yapıştırın (Ctrl+V) ya da pencereye bırakın.'
} satisfies Record<string, string>;
