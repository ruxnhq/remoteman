// What a computer's pairing code (the QR code in "Pair a Device") says, as a link:
//   remoteman://pair?v=0&name=<its name>&addr=<address[:port]>[,<address[:port]>…]&fp=<its key's fingerprint>
// The device that scans it adds the computer at the first address and connects with pairing on;
// the computer still has to be in pairing mode, and where it confirms new devices it still does.
// The fingerprint lets the device refuse any other computer that answers at that address.
import Foundation

struct PairLink: Equatable {
    var name: String
    var addresses: [String]
    var fingerprint: String

    var url: String {
        var c = URLComponents()
        c.scheme = "remoteman"
        c.host = "pair"
        c.queryItems = [
            URLQueryItem(name: "v", value: "0"), URLQueryItem(name: "name", value: name),
            URLQueryItem(name: "addr", value: addresses.joined(separator: ",")), URLQueryItem(name: "fp", value: fingerprint),
        ]
        return c.string ?? ""
    }

    init(name: String, addresses: [String], fingerprint: String) {
        self.name = name
        self.addresses = addresses
        self.fingerprint = fingerprint
    }

    /// Nil unless this is a remoteman pairing link with an address and a whole fingerprint.
    init?(_ text: String) {
        guard let c = URLComponents(string: text.trimmingCharacters(in: .whitespacesAndNewlines)), c.scheme == "remoteman", c.host == "pair" else { return nil }
        func item(_ n: String) -> String { c.queryItems?.first { $0.name == n }?.value ?? "" }
        let addresses = item("addr").split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
        let fp = item("fp").lowercased()
        guard !addresses.isEmpty, fp.count >= 32, fp.allSatisfy({ $0.isHexDigit }) else { return nil }
        name = item("name")
        self.addresses = addresses
        fingerprint = fp
    }
}
