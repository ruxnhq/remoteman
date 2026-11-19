// The remoteman camera: a CoreMediaIO camera extension with one device, "remoteman Camera".
// Apps on this Mac read its source stream; the host app (remoteman-host) writes the connected
// device's camera pictures into its sink stream, and they are passed on as they come. With
// nothing from the host app, a dark still frame keeps the camera running.

import CoreMediaIO
import Foundation

let providerSource = ProviderSource(clientQueue: nil)
CMIOExtensionProvider.startService(provider: providerSource.provider)
CFRunLoopRun()
