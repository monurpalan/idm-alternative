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
	'Link may have expired': 'Linkin süresi dolmuş olabilir',
	'Link expires soon': 'Linkin süresi dolmak üzere',
	'Waiting for the network': 'Ağ bekleniyor',
	Paused: 'Duraklatıldı',
	'Moving {p}%': 'Taşınıyor %{p}',
	'Login needed': 'Giriş gerekli',
	'Link expired': 'Bağlantı eskidi',
	'Not found': 'Bulunamadı',
	'No downloads here': 'Burada indirme yok',
	'Paste a link with Ctrl+V, drop one on the window, or choose New download.':
		'Ctrl+V ile bir bağlantı yapıştırın, pencereye bırakın ya da Yeni indirme’yi seçin.',
	'Fold the group': 'Grubu katla',
	'Show the group': 'Grubu aç',
	'{done} of {n} done': '{done}/{n} bitti',
	'Rec. {time}': 'Kayıt {time}',
	'Starts at {time}': '{time} başlar',

	// Where did this file come from?
	'Downloaded here, by Shardload.': 'Shardload buraya indirdi.',
	'The same file as one Shardload downloaded (same size and content).':
		'Shardload’un indirdiği bir dosyayla aynı (aynı boyut ve içerik).',
	'Not downloaded by Shardload; the browser noted where it came from.':
		'Shardload indirmedi; tarayıcı nereden geldiğini not etmiş.',
	'Nothing says where this file came from.': 'Bu dosyanın nereden geldiğini söyleyen bir şey yok.',
	'Where did this file come from?': 'Bu dosya nereden geldi?',
	Address: 'Adres',
	Page: 'Sayfa',
	Checksum: 'Sağlama toplamı',
	'There is no checksum to check it against, and the publisher posts none.':
		'Karşılaştırılacak bir sağlama toplamı yok; yayıncı da yayımlamıyor.',
	'The file matches its checksum ({from}).': 'Dosya sağlama toplamıyla eşleşiyor ({from}).',
	'The file does not match its checksum ({from}): it changed or is damaged.':
		'Dosya sağlama toplamıyla eşleşmiyor ({from}): değişmiş ya da bozulmuş.',
	'A newer release is out: {latest} (this file is from {release}).':
		'Daha yeni bir sürüm çıkmış: {latest} (bu dosya {release} sürümünden).',
	'This is the newest release ({release}).': 'Bu en yeni sürüm ({release}).',
	'Downloading it again takes what is unchanged from this copy (zsync): only the parts that changed come over the network.':
		'Yeniden indirmek değişmeyen kısımları bu kopyadan alır (zsync): ağdan yalnız değişen kısımlar gelir.',
	'The address serves a different file now ({size}).':
		'Adres artık başka bir dosya veriyor ({size}).',
	'The address serves the same file as when it was downloaded.':
		'Adres, indirildiği zamanki dosyanın aynısını veriyor.',
	'The server does not say whether it changed.': 'Sunucu değişip değişmediğini söylemiyor.',
	'Added to the list.': 'Listeye eklendi.',
	'Shardload has not downloaded {name}.': 'Shardload {name} dosyasını indirmedi.',
	'Choose the file: Windows keeps the address it was downloaded from with it, and so does Linux.':
		'Dosyayı seçin: Windows (ve Linux) indirildiği adresi dosyayla birlikte saklar.',
	'Choose a file to see where it came from.': 'Nereden geldiğini görmek için bir dosya seçin.',
	'Choose a file…': 'Dosya seç…',
	'Another file…': 'Başka bir dosya…',
	'Check the checksum': 'Sağlama toplamını denetle',
	'Newer version?': 'Yeni sürüm var mı?',
	'Where did it come from?': 'Nereden geldi?',
	// Disk space
	'Disk full': 'Disk dolu',
	'Not enough disk space': 'Yeterli disk alanı yok',
	'{missing} short on {volume}': '{volume} üzerinde {missing} eksik',
	'{need} to download, {free} free, {reserve} kept free': '{need} indirilecek, {free} boş, {reserve} boş tutulur',
	'and {n} of unknown size': 've boyutu bilinmeyen {n} indirme',
	'Started anyway, downloads pause by themselves before the drive fills up.':
		'Yine de başlatılırsa indirmeler sürücü dolmadan kendiliğinden duraklar.',
	'Start anyway': 'Yine de başlat',
	'Move to another drive…': 'Başka bir sürücüye taşı…',
	'{n} downloads paused: {volume} has {free} left. Free some space or move them, then resume.':
		'{n} indirme duraklatıldı: {volume} üzerinde {free} kaldı. Yer açın ya da taşıyın, sonra sürdürün.',
	'Paused before the drive filled up: free some space or move it to another drive, then resume.':
		'Sürücü dolmadan duraklatıldı: yer açın ya da başka bir sürücüye taşıyın, sonra sürdürün.'
} satisfies Record<string, string>;
