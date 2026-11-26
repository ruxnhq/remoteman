// The tests of apple/Mac/UpdateCheck.swift, as a program (the Xcode project has no test target):
//
//     swiftc apple/Mac/UpdateCheck.swift apple/Tests/update-check/main.swift -o /tmp/update-check && /tmp/update-check
//
// Run from the repository's top folder: one test reads site/public/index.html.
import Foundation

var failures = 0
func check(_ ok: Bool, _ what: String, line: Int = #line) {
    if !ok {
        failures += 1
        print("FAIL line \(line): \(what)")
    }
}
func v(_ text: String) -> UpdateVersion { UpdateVersion(text)! }

// Versions compare as numbers.
check(v("0.1.10") > v("0.1.9"), "0.1.10 > 0.1.9")
check(v("0.2.0") > v("0.1.99"), "0.2.0 > 0.1.99")
check(v("1.0.0") > v("0.99.99"), "1.0.0 > 0.99.99")
check(v("0.2") == v("0.2.0"), "0.2 == 0.2.0")
check(v("0.1.7").description == "0.1.7", "0.1.7 written back")
for bad in ["", "0..1", "0.1.x", "v0.1.7", "0.1.7-beta", " 0.1.7", "1.2.3.4.5", "0.1.1234567890", "٣.1"] {
    check(UpdateVersion(bad) == nil, "\(bad.debugDescription) is no version")
}

// The newest disk image is read from the page.
let page = """
    <a href="https://files.ruxn.com/p/remoteman/remoteman-viewer-0.1.9-macos.dmg">Viewer</a>
    <a href="https://files.ruxn.com/p/remoteman/remoteman-host-0.1.12-macos.dmg">Host</a>
    <a href="https://files.ruxn.com/p/remoteman/remoteman-viewer-0.1.10-macos.dmg.sha256">viewer</a>
    <a href="https://files.ruxn.com/p/remoteman/remoteman-viewer-setup-0.1.11.exe">Windows</a>
    <a href="https://files.ruxn.com/p/remoteman/remoteman-viewer-0.1.13-x86_64.AppImage">Linux</a>
    <a href="https://example.com/p/remoteman/remoteman-viewer-9.9.9-macos.dmg">elsewhere</a>
    """
switch UpdateCheck.newest(onPage: page) {
case .success(let r):
    check(r.version == v("0.1.10"), "newest is 0.1.10, not \(r.version)")
    check(r.file == "remoteman-viewer-0.1.10-macos.dmg", r.file)
    check(r.url.absoluteString == "https://files.ruxn.com/p/remoteman/remoteman-viewer-0.1.10-macos.dmg", r.url.absoluteString)
    check(r.sha256URL.absoluteString == "https://files.ruxn.com/p/remoteman/remoteman-viewer-0.1.10-macos.dmg.sha256", r.sha256URL.absoluteString)
case .failure(let f): check(false, f.words)
}
if case .failure(let f) = UpdateCheck.newest(onPage: "<html>moved</html>") {
    check(f.words.contains("https://remoteman.ruxn.com/") && f.words.contains("has the page changed"), f.words)
} else { check(false, "a page without a link is an error") }
if case .success = UpdateCheck.newest(onPage: "https://files.ruxn.com/p/remoteman/remoteman-viewer-latest-macos.dmg") {
    check(false, "latest is no version")
}

// The page as the repository has it now: whatever it says, it must still be read.
if let site = try? String(contentsOfFile: "site/public/index.html", encoding: .utf8) {
    switch UpdateCheck.newest(onPage: site) {
    case .success(let r):
        check(site.contains("href=\"\(r.url.absoluteString)\""), r.url.absoluteString)
        check(site.contains(r.sha256URL.absoluteString), r.sha256URL.absoluteString)
    case .failure(let f): check(false, f.words)
    }
} else {
    print("note: site/public/index.html not found (run from the repository's top folder); that test is left out")
}

// .sha256 files are read.
let hash = "9F86D081884C7D659A2FEAA0C55AD015A3BF4F1B2B0B822CD15D6C15B0F00A08"
let file = "remoteman-viewer-0.1.8-macos.dmg"
check(UpdateCheck.sha256(of: "\(hash)  \(file)\n", file: file) == .success(hash.lowercased()), "plain")
check(UpdateCheck.sha256(of: "\u{feff}\(hash) *\(file)\r\n", file: file) == .success(hash.lowercased()), "BOM, star, CRLF")
check(UpdateCheck.sha256(of: "\n\(hash)\n", file: file) == .success(hash.lowercased()), "no name")
if case .failure(let f) = UpdateCheck.sha256(of: "\(hash)  remoteman-viewer-0.1.7-macos.dmg\n", file: file) {
    check(f.words.contains("0.1.7"), f.words)
} else { check(false, "another file's hash") }
if case .success = UpdateCheck.sha256(of: "\(hash.prefix(63))  \(file)\n", file: file) { check(false, "63 digits") }
if case .success = UpdateCheck.sha256(of: "<html>not found</html>", file: file) { check(false, "a page") }
if case .success = UpdateCheck.sha256(of: "", file: file) { check(false, "empty") }

// Old downloads go.
let names = ["remoteman-viewer-0.1.0-macos.dmg", "remoteman-viewer-0.1.1-macos.dmg", "remoteman-viewer-0.1.2-macos.dmg",
    "remoteman-viewer-0.1.3-macos.dmg", "remoteman-viewer-0.1.3-macos.dmg.part", "update.log", "remoteman-viewer-notes.txt"]
check(UpdateCheck.stale(names, current: v("0.1.1")) == ["remoteman-viewer-0.1.0-macos.dmg", "remoteman-viewer-0.1.1-macos.dmg", "remoteman-viewer-0.1.3-macos.dmg.part"], "at the start of 0.1.1")
check(UpdateCheck.stale(names, current: v("0.1.1"), keep: "remoteman-viewer-0.1.3-macos.dmg") == ["remoteman-viewer-0.1.0-macos.dmg", "remoteman-viewer-0.1.1-macos.dmg", "remoteman-viewer-0.1.2-macos.dmg", "remoteman-viewer-0.1.3-macos.dmg.part"], "0.1.3 just downloaded")

// States in words.
let this = v("0.1.1")
check(UpdateState.available(v("0.1.2")).words(this: this) == "Update available: this Mac has 0.1.1, and installing gets 0.1.2.", "available")
check(UpdateState.upToDate(v("0.1.1")).words(this: this) == "No update: this Mac has 0.1.1, and the newest release is 0.1.1.", "up to date")
check(UpdateState.failed("no network").words(this: this) == "Failed: no network.", "failed")
check(UpdateState.downloading(v("0.1.2")).newer == v("0.1.2") && UpdateState.upToDate(this).newer == nil, "newer")

print(failures == 0 ? "update-check: all tests passed" : "update-check: \(failures) failed")
exit(failures == 0 ? 0 : 1)
