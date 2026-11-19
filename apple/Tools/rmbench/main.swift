// rmbench: time hardware decode of recorded streams.
//   rmbench [--flat-out] [--loops N] file.rmrec ...
import Foundation

var paced = true
var loops = 1
var files: [String] = []
var args = Array(CommandLine.arguments.dropFirst())
while !args.isEmpty {
    let a = args.removeFirst()
    switch a {
    case "--flat-out": paced = false
    case "--loops": loops = Int(args.isEmpty ? "1" : args.removeFirst()) ?? 1
    default: files.append(a)
    }
}
if files.isEmpty {
    print("usage: rmbench [--flat-out] [--loops N] file.rmrec ...")
    exit(2)
}
for f in files { print(runBench(URL(fileURLWithPath: f), paced: paced, loops: loops)) }
