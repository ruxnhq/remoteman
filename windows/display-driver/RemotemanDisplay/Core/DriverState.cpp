#include "DriverState.h"

std::mutex g_Mutex;
std::mutex g_DataMutex; // Protects monitorModes, display colour settings, numVirtualDisplays, gpuname
std::mutex g_CursorSettingsMutex; // Publishes cursor capability fields as one snapshot
WDFDEVICE g_GlobalDevice = nullptr;

DriverOptions Options;
std::vector<std::tuple<int, int, int, int>> monitorModes;
UINT numVirtualDisplays = 1;
std::wstring gpuname = L"";
// remoteman: its own folder, %ProgramData%\remoteman\display, which remoteman host's installer
// fills, rather than zako-vdd's C:\VirtualDisplayDriver, which other forks share.
std::wstring confpath = []()
{
	wchar_t programData[MAX_PATH];
	DWORD len = GetEnvironmentVariableW(L"ProgramData", programData, MAX_PATH);
	if (len == 0 || len > MAX_PATH)
	{
		return std::wstring(L"C:\\ProgramData\\remoteman\\display");
	}
	return std::wstring(programData) + L"\\remoteman\\display";
}();

std::atomic<bool> HDRPlus{false};
std::atomic<bool> SDR10{false};
std::atomic<bool> customEdid{false};
std::atomic<bool> dolbyVision{false};

// Variable Refresh Rate (FreeSync / G-Sync compatible) toggle. Keep the
// setting for UI/IOCTL compatibility, but do not infer adapter capability
// flags from it on the current IddCx0102 compatibility path. Some hosts
// reject unknown or mismatched adapter flag bits during adapter init.
std::atomic<bool> vrrEnabled{false};
std::atomic<bool> hardwareCursor{false};
std::atomic<bool> preventManufacturerSpoof{false};
std::atomic<bool> edidCeaOverride{false};
std::atomic<bool> legacyNamedFrameChannel{false};
std::atomic<bool> frameChannelWanted{false};
// Mouse settings
std::atomic<bool> alphaCursorSupport{true};
int CursorMaxX = 128;
int CursorMaxY = 128;
IDDCX_XOR_CURSOR_SUPPORT XorCursorSupportLevel = IDDCX_XOR_CURSOR_SUPPORT_FULL;

// Rest
IDDCX_BITS_PER_COMPONENT SDRCOLOUR = IDDCX_BITS_PER_COMPONENT_8;
IDDCX_BITS_PER_COMPONENT HDRCOLOUR = IDDCX_BITS_PER_COMPONENT_10;

std::wstring ColourFormat = L"RGB";
