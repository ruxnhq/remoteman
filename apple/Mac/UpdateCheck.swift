// Updates, the part that needs nothing but Foundation: where the newest viewer is read from, how
// versions compare, the `.sha256` beside each file, and which downloads can go. Updater.swift
// does the rest (asking, downloading, checking the signature, putting the new app in place). The
// same rules as the Windows host's (crates/remoteman-host-windows/src/update.rs), for the Mac
// viewer's disk image. Its tests run on any Mac, without Xcode's project:
//
//     swiftc apple/Mac/UpdateCheck.swift apple/Tests/update-check/main.swift -o /tmp/update-check && /tmp/update-check
import Foundation

enum UpdateSource {
    /// The page remoteman's downloads are on (built from `site/public/index.html`). There is no
    /// list of releases to read (no `latest.json`), so the newest viewer is the disk image this
    /// page links: see `newest(onPage:)`.
    static let page = URL(string: "https://remoteman.ruxn.com/")!
    /// Where the files are, each with `NAME.sha256` beside it.
    static let files = "https://files.ruxn.com/p/remoteman/"
    /// The viewer's disk image is `remoteman-viewer-X.Y.Z-macos.dmg`.
    static let prefix = "remoteman-viewer-"
    static let suffix = "-macos.dmg"
    /// The app inside it.
    static let app = "remoteman.app"
    /// Whose Developer ID the released app is signed with (`DEVELOPMENT_TEAM` in project.yml),
    /// and its bundle identifier: a downloaded app that isn't both is not installed.
    static let team = "R82PLAJ45H"
    static let identifier = "com.xnixy.remoteman.viewer"
}

/// A version as remoteman numbers its releases: numbers between dots, compared as numbers
/// (0.1.10 is after 0.1.9), with missing ones as 0 (0.2 is 0.2.0).
struct UpdateVersion: Comparable, CustomStringConvertible {
    let parts: [UInt64]

    init?(_ text: String) {
        let words = text.split(separator: ".", omittingEmptySubsequences: false)
        guard !words.isEmpty, words.count <= 4,
            words.allSatisfy({ !$0.isEmpty && $0.count <= 9 && $0.allSatisfy { $0.isASCII && $0.isNumber } })
        else { return nil }
        parts = words.map { UInt64($0) ?? 0 }
    }

    private func at(_ i: Int) -> UInt64 { i < parts.count ? parts[i] : 0 }

    static func < (a: UpdateVersion, b: UpdateVersion) -> Bool {
        for i in 0..<max(a.parts.count, b.parts.count) where a.at(i) != b.at(i) { return a.at(i) < b.at(i) }
        return false
    }

    static func == (a: UpdateVersion, b: UpdateVersion) -> Bool { !(a < b) && !(b < a) }

    var description: String { parts.map(String.init).joined(separator: ".") }
}

/// A viewer disk image that can be downloaded.
struct UpdateRelease: Equatable {
    let version: UpdateVersion
    /// `remoteman-viewer-X.Y.Z-macos.dmg`.
    let file: String
    let url: URL

    init(_ version: UpdateVersion) {
        self.version = version
        file = UpdateSource.prefix + version.description + UpdateSource.suffix
        url = URL(string: UpdateSource.files + file)!
    }

    /// Its SHA-256, in a file of its own beside it.
    var sha256URL: URL { URL(string: url.absoluteString + ".sha256")! }
}

enum UpdateCheck {
    /// The newest viewer disk image that remoteman's download page links: every
    /// `https://files.ruxn.com/p/remoteman/remoteman-viewer-X.Y.Z-macos.dmg` on it, the highest
    /// X.Y.Z. This is the one place that knows where releases are listed.
    static func newest(onPage html: String) -> Result<UpdateRelease, UpdateFailure> {
        let link = UpdateSource.files + UpdateSource.prefix
        var newest: UpdateVersion?
        var rest = html[...]
        while let r = rest.range(of: link) {
            rest = rest[r.upperBound...]
            if let end = rest.range(of: UpdateSource.suffix), let v = UpdateVersion(String(rest[..<end.lowerBound])) {
                if newest.map({ v > $0 }) ?? true { newest = v }
            }
        }
        guard let newest else {
            return .failure(UpdateFailure(
                "\(UpdateSource.page.absoluteString) links no \(UpdateSource.prefix)X.Y.Z\(UpdateSource.suffix) in \(UpdateSource.files), so the newest version can't be told (has the page changed?)"))
        }
        return .success(UpdateRelease(newest))
    }

    /// The SHA-256 a `.sha256` file gives for `file`, in lower-case hex. The file is one line,
    /// `<64 hex digits>  <file name>` (as `shasum -a 256` writes it; the name may be left out).
    static func sha256(of text: String, file: String) -> Result<String, UpdateFailure> {
        let trimmed = text.hasPrefix("\u{feff}") ? String(text.dropFirst()) : text
        guard let line = trimmed.split(whereSeparator: \.isNewline).map({ $0.trimmingCharacters(in: .whitespaces) }).first(where: { !$0.isEmpty })
        else { return .failure(UpdateFailure("the .sha256 file is empty")) }
        let words = line.split(whereSeparator: { $0 == " " || $0 == "\t" })
        let hash = words.first.map(String.init) ?? ""
        guard hash.count == 64, hash.allSatisfy(\.isHexDigit) else {
            return .failure(UpdateFailure("the .sha256 file doesn't start with a SHA-256 (64 hex digits): \(line.prefix(80))"))
        }
        // `shasum -b` marks the name with a star.
        if words.count > 1 {
            let name = String(words[1].drop(while: { $0 == "*" }))
            if name != file { return .failure(UpdateFailure("the .sha256 file is for \(name), not \(file)")) }
        }
        return .success(hash.lowercased())
    }

    /// What the updates folder holds that can go: unfinished downloads, disk images of this
    /// version or older ones, and, when one has just been downloaded (`keep`), every other.
    static func stale(_ names: [String], current: UpdateVersion, keep: String? = nil) -> [String] {
        names.filter { name in
            guard name.hasPrefix(UpdateSource.prefix) else { return false }
            if name.hasSuffix(".part") { return true }
            guard name.hasSuffix(UpdateSource.suffix) else { return false }
            let middle = name.dropFirst(UpdateSource.prefix.count).dropLast(UpdateSource.suffix.count)
            guard let v = UpdateVersion(String(middle)) else { return false }
            return v <= current || (keep != nil && keep != name)
        }
    }
}

/// Why looking for or installing an update didn't work, in words for the person.
struct UpdateFailure: Error, Equatable {
    let words: String
    init(_ words: String) { self.words = words }
}

/// Where the looking for updates stands, as Settings and the window say it.
enum UpdateState: Equatable {
    /// Not looked yet since the app started.
    case notYet
    case checking
    /// Nothing newer: the newest release on the download page is that one.
    case upToDate(UpdateVersion)
    case available(UpdateVersion)
    case downloading(UpdateVersion)
    /// The new app is being checked and put in place; remoteman quits and opens again.
    case installing(UpdateVersion)
    case failed(String)

    /// A newer version this Mac could get, while there is one.
    var newer: UpdateVersion? {
        switch self {
        case .available(let v), .downloading(let v), .installing(let v): return v
        default: return nil
        }
    }

    var busy: Bool {
        switch self {
        case .checking, .downloading, .installing: return true
        default: return false
        }
    }

    /// The state as said to the person, for an app of version `this`.
    func words(this: UpdateVersion) -> String {
        switch self {
        case .notYet: return "Not looked for yet."
        case .checking: return "Looking for updates…"
        case .upToDate(let newest): return "No update: this Mac has \(this), and the newest release is \(newest)."
        case .available(let v): return "Update available: this Mac has \(this), and installing gets \(v)."
        case .downloading(let v): return "Downloading \(v)…"
        case .installing(let v): return "Installing \(v): remoteman quits and opens again."
        case .failed(let why): return "Failed: \(why)" + (why.hasSuffix(".") ? "" : ".")
        }
    }
}
