# The one FFmpeg build that Glossplay is released with: gyan.dev "essentials", a static
# GPL v3 build for Windows x64 with libx264, libmp3lame, libopus, libass, libwebp and https
# through GnuTLS. The GitHub release of a version is permanent, unlike a daily build.
# fetch-ffmpeg.ps1 downloads exactly this and package.ps1 refuses to ship anything else.
$FfmpegVersion = '8.1.1'
$FfmpegArchiveName = "ffmpeg-$FfmpegVersion-essentials_build.7z"
$FfmpegArchiveSha256 = '23ad8969fbe701d44e6e7e2b97c5fae4a71224fc33a2560a9034e5110d029d15'
$FfmpegExeSha256 = '228d7a8556258de907fdb55f36850078ebc7680b84ec30d84ea02e99bec1d1eb'
$FfmpegArchiveUrl = "https://github.com/GyanD/codexffmpeg/releases/download/$FfmpegVersion/$FfmpegArchiveName"
$FfmpegSourceCommit = '239f2c733d'
