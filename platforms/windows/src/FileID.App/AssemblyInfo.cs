// Expose internal types to the FileID.App.Tests project so xUnit can
// exercise services that don't need to be public.

using System.Runtime.CompilerServices;

[assembly: InternalsVisibleTo("FileID.App.Tests")]
