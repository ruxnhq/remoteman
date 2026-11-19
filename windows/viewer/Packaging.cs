namespace Remoteman;

/// Whether this process runs from an MSIX package (the Store's) or from a plain folder.
static partial class Packaging
{
    [System.Runtime.InteropServices.LibraryImport("kernel32.dll")]
    private static unsafe partial int GetCurrentPackageFullName(ref uint length, char* name);

    public static unsafe bool IsPackaged
    {
        get
        {
            uint length = 0;
            // 15700, APPMODEL_ERROR_NO_PACKAGE: not packaged. 122, too small a buffer: packaged.
            return GetCurrentPackageFullName(ref length, null) != 15700;
        }
    }
}
