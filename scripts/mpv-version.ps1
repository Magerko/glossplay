# The one libmpv build that LinguaPlay is released with. fetch-mpv.ps1 downloads exactly
# this build and package.ps1 refuses to ship anything else.
$MpvBuildTag = '20261111'
$MpvMpvCommit = 'bcde6b93cc'
$MpvArchiveName = 'mpv-dev-x86_64-20261111-git-bcde6b93cc.7z'
$MpvArchiveSha256 = '273c079b11ea5575822f972756f4c9a71149a5c10130353abeb8586738938452'
$MpvDllSha256 = '88504ee5560942a955405e99dfb5971368e4cf851321f36565bf2a667271db1b'
$MpvArchiveUrl = "https://github.com/shinchiro/mpv-winbuild-cmake/releases/download/$MpvBuildTag/$MpvArchiveName"
