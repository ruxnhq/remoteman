# remoteman for Android and Android TV on Google Play

RUXN LLC publishes the app on Google Play as `com.ruxn.remoteman`, one listing for phones,
tablets and TVs. Testers get builds from Play's internal testing track, not by sideloading.
Facts about Play below were read from Google's pages on 2026-11-23; the links are there to
check them again.

## What is in the repository

- **Icon and banner**: `make-icons.sh` cuts them from `assets/icon/viewer.svg`, the master
  for every platform. The launcher icon is adaptive (`res/mipmap-anydpi/ic_launcher.xml`): the
  mark on a transparent layer inside the 66 dp safe circle, the master's blue `#2458F5` behind
  it, and the same layer as the monochrome one for themed icons. The TV banner
  (`res/drawable-xhdpi/banner.png`, 320×180, `android:banner`) is the mark and the name in Noto
  Sans Bold. `play/` holds the listing's pictures: `icon-512.png`,
  `feature-graphic-1024x500.png` and `tv-banner-1280x720.png`.
- **Release build**: `./gradlew bundleRelease` makes
  `app/build/outputs/bundle/release/app-release.aab`. R8 shrinks the code and resources.
  `app/proguard-rules.pro` keeps what the Rust side calls by name: `Core`'s native methods, and
  `Core.Listener`'s `on…` methods on whatever implements it. The Rust library goes in stripped,
  and its symbol table goes in the bundle's metadata for Play's crash reports.
- **Version**: `gradle.properties` (`remoteman.versionName`, `remoteman.versionCode`) is the
  only place it is set. Play refuses a bundle whose `versionCode` isn't higher than every
  earlier upload, on any track, so raise it by one for each upload.
- **Upload key**: read from `~/.gradle/gradle.properties` or the environment, never from the
  repository. `*.jks` and `*.keystore` are ignored by git. Without the key the release build is
  unsigned, and Play won't take it.
- `bundletool.sh` runs bundletool from Google's Maven repository through Gradle. It turns the
  bundle into the APKs Play would serve a given device.

## Building a release

```sh
android/build-core.sh                          # the Rust side, release
# raise remoteman.versionCode (and versionName if it's a new version) in android/gradle.properties
cd android && JAVA_HOME=/opt/android-studio/jbr ./gradlew bundleRelease
```

Checked on 2026-11-23 for 0.1.0 (1), a 21 MB bundle:
- Both ABIs (arm64-v8a, x86_64) and every native library in the bundle, ML Kit's and CameraX's
  included, have no `.debug_*` sections, and their LOAD segments are aligned to 16 KB
  (`readelf -lW`). Play requires 16 KB page support of apps that target Android 15 or later,
  and of TV apps since 1 August 2026. From 1 February 2027 it refuses updates that lack it.
- `bundletool build-apks`, then `install-apks` on a phone emulator (API 37) and a Google TV
  emulator (API 36). Play would deliver about 16 MB to them as splits: base, x86_64, density
  and language.
- Both start. The TV opens on the list with D-pad focus on "Add a computer". The phone's
  launcher shows the icon whole inside its circle mask. `aapt2 dump badging` shows
  `banner='res/drawable-xhdpi-v4/banner.png'`, the adaptive icon, and a leanback-launchable
  activity. The emulator's Google TV home screen needs a Google sign-in, so the banner wasn't
  seen there.
- On the phone, the R8 build tried a connection to an address where nothing answered. Rust's
  calls into `Core.Listener` reached the screen by name ("Can't connect to 10.0.2.2:7799").
  R8's mapping shows all five `on…` methods kept. A full session (pairing code, picture) wasn't
  run on the R8 build, because no test host was up. Run one before the first upload.
- The scanner opens on the R8 build and shows the emulator's camera. No code was read there.
  At first it crashed: R8's full mode had dropped the constructors ML Kit calls by reflection,
  and `proguard-rules.pro` now keeps them. Try every screen once on a release build after
  adding a library.

To try a bundle on a device or emulator the way Play cuts it:

```sh
android/bundletool.sh build-apks --bundle=android/app/build/outputs/bundle/release/app-release.aab \
    --output=/tmp/remoteman.apks --overwrite --aapt2=$HOME/Android/Sdk/build-tools/36.0.0/aapt2 \
    --ks=$HOME/.android/debug.keystore --ks-key-alias=androiddebugkey --ks-pass=pass:android
android/bundletool.sh install-apks --apks=/tmp/remoteman.apks --device-id=emulator-5554 \
    --adb=$HOME/Android/Sdk/platform-tools/adb
```

## The upload key

Play App Signing keeps the key that signs what users install. The upload key only proves to
Play that an upload comes from RUXN. It was made on 2026-11-23 on the owner's PC at
`~/.config/ruxn/remoteman-upload.jks` (RSA 4096, alias `upload`, valid until 2054, owner
`CN=RUXN LLC, O=RUXN LLC`), with its random password in `~/.gradle/gradle.properties`. The
certificate's SHA-256 fingerprint, to compare with the upload key Play Console shows:
`15:DE:98:F8:D9:2A:37:5E:1E:0A:3C:A8:65:BE:A6:D0:D3:0A:81:D8:AE:17:82:1A:97:44:15:77:C6:E1:AF:46`.

On another machine, copy both, or make a new key the same way (after Google has reset the
upload key, if the old one is lost):

```sh
mkdir -p ~/.config/ruxn
keytool -genkeypair -v -keystore ~/.config/ruxn/remoteman-upload.jks -alias upload \
    -keyalg RSA -keysize 4096 -validity 10000 -dname "CN=RUXN LLC, O=RUXN LLC"
```

Then, in `~/.gradle/gradle.properties` (or as environment variables of the same names):

```properties
REMOTEMAN_UPLOAD_STORE_FILE=/home/you/.config/ruxn/remoteman-upload.jks
REMOTEMAN_UPLOAD_STORE_PASSWORD=…
REMOTEMAN_UPLOAD_KEY_ALIAS=upload
REMOTEMAN_UPLOAD_KEY_PASSWORD=…
```

**Back up the `.jks` file and its password** in a password manager or another safe place. If
the key is lost or leaked, the account owner has to ask Google to reset the upload key, and
nothing can be uploaded until Google does. The app's own key is with Google and isn't affected.

## What the owner does in Play Console

These steps are the owner's: Google accounts, agreements, payment and uploads aren't done
for you.

1. **Developer account for RUXN LLC**: an organization account
   (<https://play.google.com/console/signup>). It needs RUXN's D-U-N-S number (free from Dun &
   Bradstreet, and it can take some days if RUXN doesn't have one yet), the US$25 one-time
   fee, and possibly a government ID and a credit card in the legal name. Personal accounts
   have to run a 12-tester, 14-day closed test before production, and Google states that rule
   for personal accounts only. Internal testing has no such requirement.
2. **Create the app**: name "remoteman", default language English (US), App, Free. Accept the
   declarations.
3. **Turn on Android TV**: Test and release → Setup → Advanced settings → Form factors → Add
   release type → Android TV, then accept the TV review terms. The TV listing needs at least
   one TV screenshot (1920×1080, unaltered), the TV banner (`play/tv-banner-1280x720.png`), and
   "Android TV" mentioned in the description.
4. **App content** (Policy → App content):
   - Privacy policy: `https://remoteman.ruxn.com/privacy/`. Play also wants it linked inside
     the app, and the app doesn't link it yet (see "What each step needs" below).
   - Ads: no ads.
   - App access: no sign-in. Say in the notes that the app connects to the user's own computer
     running the remoteman host, with a link to the downloads at <https://remoteman.ruxn.com>.
     A reviewer with no host only sees the list and the "can't connect" screen.
   - Target audience: 18 and over (the app isn't directed at children; the privacy policy says
     so).
   - Content rating: the IARC questionnaire. Category: utility/productivity. No violence,
     sexual content, gambling or user-to-user communication. The app shows the user's own
     computer screen and shares no content between users.
   - Data safety: see the next section. Google exempts apps that are only on internal testing,
     but the form is needed before any closed, open or production release.
5. **Internal testing** (Test and release → Testing → Internal testing):
   - Testers tab: make an email list with the tester's Google account address (up to 100
     testers). Save.
   - Create a release: let Google manage the app signing key (Play App Signing), upload
     `app-release.aab`, write a note, save, then review and roll out.
   - Send the tester the opt-in link from the Testers tab. They open it while signed in with
     that Google account, accept, then install "remoteman" from the Play Store on the phone.
     Google shows the opt-in link only once the app's status is "Published". While the app is
     still a draft, send the tester the app's Play Store address instead.
     For the TV, the same account installs it from the Play Store on the TV. Google's pages
     don't say whether internal-track builds reach a TV this way, so ask the tester to check.
   - Builds normally reach testers within minutes. The first upload of an app can take a few
     hours before the link works.

## What each step needs

- **Internal testing** needs the developer account, the app, the upload key and a bundle.
  Google exempts apps that are only on internal testing from the Data safety section, and an
  internal test can start before the app's setup is complete. Run a full session on a release
  build first (pairing, picture, input), on a phone emulator and the TV emulator, against a
  test host.
- **Closed testing, open testing or production** also need the two items below, the App
  content declarations, the Data safety form and the TV review.

- **The privacy policy inside the app**: Play wants a link in the app as well as on the
  listing. Settings needs a "Privacy policy" row that opens `https://remoteman.ruxn.com/privacy/`.
- **ML Kit and the Data safety answers**: the camera's code reader is ML Kit's barcode
  scanner, with the model bundled in the app. The pictures stay on the device. Google's ML Kit
  terms say the SDK "also send[s] metrics about the performance and utilization of the APIs"
  to Google, and the release build does include its sender
  (`com.google.android.datatransport`, `transport-backend-cct`). Google documents no way to
  turn it off. Play counts data that an SDK sends as collected by the app. Choose one:
  - **Keep ML Kit**: answer Data safety as in A below, and change the privacy policy. It now
    says the apps contain "no advertising or third-party tracking code", and ML Kit's terms
    make RUXN responsible for telling users about Google's processing of these metrics.
  - **Replace it with a reader that sends nothing** (ZXing's core library, Apache 2.0, used
    through CameraX's image analysis): answer as in B. The app also gets about 5 MB smaller.

## Data safety answers (draft)

What the app handles:
- **The session** (the picture, sound, input and pairing) goes directly between the device and
  the user's own computer. It is encrypted end to end with TLS 1.3 over QUIC, and RUXN has no
  server in between and no keys. Play's rule: "User data that is sent off device, but that is
  unreadable by you or anyone other than the sender and recipient as a result of end-to-end
  encryption does not need to be disclosed."
- **Kept on the device**: the paired computers, their addresses and fingerprints, the device's
  identity key, and settings. Play's rule: data "only processed locally on the user's device
  and not sent off device does not need to be disclosed."
- **The camera** while scanning a pairing code: processed on the device only.
- **ML Kit's metrics** (only while ML Kit is in the app): device information (maker, model,
  OS version), the app's package name and version, a per-installation identifier (not meant to
  identify a user or a device), and performance and error figures. They go to Google for
  diagnostics and usage analytics, encrypted in transit (HTTPS), and Google doesn't pass them
  to third parties.

**A. With ML Kit** (the build as it is):
- Does your app collect or share any of the required user data types? **Yes.**
- Is all of the user data collected by your app encrypted in transit? **Yes.**
- Do you provide a way for users to request that their data is deleted? **No.** RUXN holds
  none, and Google holds ML Kit's metrics.
- Data types:
  - App info and performance → **Diagnostics**: collected, not shared. Not processed
    ephemerally. Collection is required. Purposes: **Analytics**.
  - Device or other IDs → **Device or other IDs**: collected, not shared. Not processed
    ephemerally. Collection is required. Purposes: **Analytics**.
- Everything else (location, personal info, messages, photos and videos, audio, files,
  contacts, app activity, web browsing): **not collected**.

**B. Without ML Kit** (a code reader that sends nothing):
- Does your app collect or share any of the required user data types? **No.** The form ends
  there, and the listing shows "No data collected" and "No data shared with third parties".

## Permissions

Play asks for a declaration form for high-risk permissions, such as SMS and call log. The
Google pages read on 2026-11-23 don't name `CAMERA` or `ACCESS_LOCAL_NETWORK` among them, but
Play's full list wasn't checked. If a permission needs a declaration, Play Console shows an
alert when the bundle is uploaded, test tracks included.

| Permission | Why | Kind |
|---|---|---|
| `INTERNET` | the connection to the computer | normal |
| `ACCESS_NETWORK_STATE` | knowing whether there is a network | normal |
| `ACCESS_LOCAL_NETWORK` | Android 17 refuses sockets to local addresses without it | runtime, asked before the first connection |
| `CAMERA` | scanning a computer's pairing code (phones) | runtime, asked when scanning; `android.hardware.camera` is not required, so TVs install it |

`com.ruxn.remoteman.DYNAMIC_RECEIVER_NOT_EXPORTED_PERMISSION` comes from AndroidX. It is a
signature permission that only the app itself holds.

## Play's requirements, and where the app stands

- **Target API level**: since 31 August 2026, new apps and updates must target Android 16
  (API 36) or higher. TV apps must target Android 14 (API 34) or higher. The app targets 37.
- **App Bundle**: required, and the release is an `.aab`.
- **64-bit and 16 KB pages**: TV apps have needed both since 1 August 2026. The bundle has
  arm64-v8a and x86_64, all 16 KB aligned.
- **32-bit devices**: there is no `armeabi-v7a` build. Many Android TV sets and boxes run a
  32-bit system and won't be offered the app. Adding one means building the Rust side for
  `armv7-linux-androideabi` and adding it to `abiFilters`.

TV app quality (<https://developer.android.com/docs/quality-guidelines/tv-app-quality>,
"TV Ready"):

| Item | State |
|---|---|
| TV-LM/ML: shows in the TV launcher (`LEANBACK_LAUNCHER`) | yes |
| TV-LB: 320×180 banner, and an icon of 160×160 or more at xhdpi | yes (the adaptive icon) |
| TV-BN: the banner has the app's name | yes |
| TV-MT: touchscreen (and camera) not required | yes |
| TV-DP: everything works with the D-pad | yes, tried by D-pad only on the Google TV emulator (README) |
| TV-DM: no Menu button needed | yes |
| TV-DB: Back leads back to the home screen | Back on the list leaves the app. In a session it opens the bar, and Disconnect then returns to the list |
| TV-LO/TR: landscape, full screen, opaque background | yes |
| TV-OV: nothing in the overscan edge | the TV screens keep a 5% margin |
| TV-PS: minSdk 31 or lower | 29 |
| TV-G1: App Bundle | yes |
| TV-G4: at least one TV screenshot | the owner's, for the listing (from the TV emulator or a TV) |
| TV-G5: sign-in details for review | no sign-in. Explain the need for a host in App access |
| TV-G6: 64-bit and 16 KB pages | yes |

## Listing text (draft)

Short description: "See and control your own computers from your phone, tablet or TV."

Full description should say what it needs: the free remoteman host on the computer (Linux,
Windows or macOS, from remoteman.ruxn.com), pairing once with a code, the same network or one
the user chooses. Also that it works on Android TV with the remote, and that nothing goes
through RUXN.
