// Turkish texts for the add part of the interface, keyed by the English text.
export default {
	// Add download
	'Add download': 'İndirme ekle',
	'From the browser, with its cookies and referrer.': 'Tarayıcıdan, çerezleri ve yönlendireniyle.',
	Address: 'Adres',
	'https://   (photo[001-100].jpg adds a batch)': 'https://   (foto[001-100].jpg toplu ekler)',
	'Asking the server…': 'Sunucuya soruluyor…',
	'Reading the torrent… (a magnet link asks peers, which can take a minute)':
		'Torrent okunuyor… (magnet linki eşlere sorulur, bir dakika sürebilir)',
	'Torrent: {n} files, {size}': 'Torrent: {n} dosya, {size}',
	'Files ({size} chosen)': 'Dosyalar ({size} seçili)',
	'Add mirrors…': 'Ayna ekle…',
	'Other addresses of the same file (one per line)': 'Aynı dosyanın diğer adresleri (her satıra bir tane)',
	'https://mirror.example.org/file.iso': 'https://ayna.example.org/dosya.iso',
	All: 'Tümü',
	'Batch: {n} downloads': 'Toplu: {n} indirme',
	'The server asks for a user name and password ({realm}).':
		'Sunucu kullanıcı adı ve parola istiyor ({realm}).',
	'The server asks for a user name and password.': 'Sunucu kullanıcı adı ve parola istiyor.',
	'Logs in as {user} (saved for this site)': '{user} olarak giriş yapılır (bu site için kayıtlı)',
	'Size unknown': 'Boyut bilinmiyor',
	'Can resume and use several connections': 'Devam ettirilebilir, birden çok bağlantı kullanılabilir',
	'Cannot resume (one connection)': 'Devam ettirilemez (tek bağlantı)',
	'User name': 'Kullanıcı adı',
	Password: 'Parola',
	'Sign in': 'Giriş yap',
	'Remember for this site': 'Bu site için hatırla',
	'Save as': 'Farklı kaydet',
	'Each from its server': 'Her biri kendi sunucusundan',
	'Name from the server': 'Sunucudaki ad',
	Folder: 'Klasör',
	'Browse…': 'Gözat…',
	'Download later': 'Sonra indir',
	'Start {n} downloads': '{n} indirmeyi başlat',
	'Start download': 'İndirmeyi başlat',

	// Links on a page
	'Links on the page': 'Sayfadaki bağlantılar',
	'Filter: .zip .pdf, or any text': 'Süz: .zip .pdf ya da herhangi bir metin',
	Filter: 'Süz',
	'Select shown': 'Gösterilenleri seç',
	None: 'Hiçbiri',
	'No links match.': 'Eşleşen bağlantı yok.',
	'{n} of {total} selected': '{n} / {total} seçili',
	'Download {n}': '{n} dosyayı indir',
	Download: 'İndir',

	// A page yt-dlp reads
	'Downloading from this site needs yt-dlp (and deno, which it uses for YouTube).':
		'Bu siteden indirmek için yt-dlp gerekir (YouTube için kullandığı deno ile birlikte).',
	'Both are downloaded from their official GitHub releases and checked against their published SHA-256.':
		'İkisi de resmi GitHub sürümlerinden indirilir ve yayımlanan SHA-256 değerleriyle doğrulanır.',
	'Setting up…': 'Kuruluyor…',
	'Set up yt-dlp': 'yt-dlp kur',
	'yt-dlp is reading the page…': 'yt-dlp sayfayı okuyor…',
	Quality: 'Kalite',
	'Audio only': 'Yalnız ses',
	'Title of the page': 'Sayfanın başlığı',

	// A stream (HLS, DASH)
	'Read the playlist first': 'Önce oynatma listesi okunmalı',
	'Protected streams cannot be downloaded': 'Korumalı yayınlar indirilemez',
	'Live streams cannot be downloaded yet': 'Canlı yayınlar henüz indirilemiyor',
	'This stream has separate audio: ffmpeg is needed': 'Bu yayının sesi ayrı: ffmpeg gerekli',
	'As offered': 'Sunulduğu gibi',
	Track: 'Parça',
	Stream: 'Yayın',
	'Reading the playlist…': 'Oynatma listesi okunuyor…',
	live: 'canlı',
	'This stream is protected (DRM). It cannot be saved in a playable form.':
		'Bu yayın korumalı (DRM). Oynatılabilir biçimde kaydedilemez.',
	'A live stream: downloading records it until you stop it.':
		'Canlı yayın: indirme, siz durdurana kadar kaydeder.',
	Audio: 'Ses',
	'Subtitles (saved next to the video)': 'Altyazılar (videonun yanına kaydedilir)',
	'Name from the address': 'Adresteki ad',
	'ffmpeg was not found: the video is saved as joined .ts segments.':
		'ffmpeg bulunamadı: video birleştirilmiş .ts parçaları olarak kaydedilir.'
} satisfies Record<string, string>;
