// Outcome class for a Restructure recommendation card — mirrors the macOS
// RestructureOutcome (Keep / Tidy / Reorganize). Derived from the engine's
// per-move Tier: Mixed → Tidy, Junk → Reorganize, Anchor → Keep.

namespace FileID.ViewModels;

internal enum RestructureOutcome
{
    Keep,
    Tidy,
    Reorganize,
}
