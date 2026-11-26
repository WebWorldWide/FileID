// Outcome of a Restructure recommendation card, derived from the engine's per-move Tier: Mixed → Tidy, Junk → Reorganize, Anchor → Keep.

namespace FileID.ViewModels;

internal enum RestructureOutcome
{
    Keep,
    Tidy,
    Reorganize,
}
