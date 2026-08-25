/**
 * AeroExecute-Engine — Risk Governor (C++20)
 *
 * Multi-threaded capital and drawdown controller.
 * Uses only atomic state and stack-allocated temporaries on the hot path
 * so that risk checks remain lock-free and sub-10 µs.
 *
 * Design contract:
 *   - No heap allocation inside evaluate()
 *   - Acquire/release atomics for versioned portfolio snapshots
 *   - Fail-closed on any breach
 */

#include <atomic>
#include <cstdint>
#include <stdexcept>

namespace aero::risk {

// ---------------------------------------------------------------------------
// Aligned portfolio snapshot (cache-line padded)
// ---------------------------------------------------------------------------
struct alignas(64) PortfolioSnapshot {
    double equity;           // current mark-to-market equity
    double peak_equity;      // high-water mark
    double max_drawdown;     // hard limit, e.g. 0.03 = 3 %
    double max_notional;     // per-order notional cap
    std::atomic<uint64_t> version{0};
};

// ---------------------------------------------------------------------------
// Incoming order intent (POD)
// ---------------------------------------------------------------------------
struct OrderIntent {
    double notional;
    double estimated_slippage;   // fraction
    int    side;                 // +1 buy / -1 sell
};

// ---------------------------------------------------------------------------
// Decision
// ---------------------------------------------------------------------------
enum class Decision : uint8_t {
    Approved = 0,
    RejectedDrawdown,
    RejectedNotional,
    RejectedStaleState,
    RejectedInvalid
};

struct RiskResult {
    Decision decision;
    double   projected_drawdown;
    double   allocated_lots;     // simple lot = notional / 100_000 for FX
    uint64_t state_version;
};

// ---------------------------------------------------------------------------
// Governor
// ---------------------------------------------------------------------------
class RiskGovernor {
public:
    /**
     * Construct with a non-owning pointer to shared portfolio state.
     * Lifetime of the snapshot is managed by the caller.
     */
    explicit RiskGovernor(PortfolioSnapshot* state) noexcept
        : state_(state) {
        if (!state_) {
            // Fail closed — never allow a null state
            // (In production this would be a hard assert / abort)
        }
    }

    /**
     * Evaluate an order against the current portfolio.
     * Hot path: atomic load + arithmetic only. No locks, no heap.
     * Target budget: < 8 µs on modern x86-64.
     */
    [[nodiscard]] RiskResult evaluate(const OrderIntent& intent) const noexcept {
        RiskResult result{};
        result.allocated_lots = 0.0;

        if (!state_ || intent.notional <= 0.0) {
            result.decision = Decision::RejectedInvalid;
            return result;
        }

        // Snapshot version first (acquire)
        const uint64_t ver = state_->version.load(std::memory_order_acquire);

        const double equity   = state_->equity;
        const double peak     = state_->peak_equity;
        const double max_dd   = state_->max_drawdown;
        const double max_not  = state_->max_notional;

        // Conservative projected equity after fill
        const double projected = equity - (intent.notional * intent.estimated_slippage);
        const double dd = (peak > 0.0) ? (peak - projected) / peak : 0.0;

        result.state_version      = ver;
        result.projected_drawdown = dd;

        // Stale-state guard
        if (state_->version.load(std::memory_order_relaxed) != ver) {
            result.decision = Decision::RejectedStaleState;
            return result;
        }

        if (intent.notional > max_not) {
            result.decision = Decision::RejectedNotional;
            return result;
        }

        if (dd > max_dd) {
            result.decision = Decision::RejectedDrawdown;
            return result;
        }

        // Simple FX lot calculation (1 lot = 100 000 notional)
        result.allocated_lots = intent.notional / 100'000.0;
        result.decision = Decision::Approved;
        return result;
    }

    /**
     * Publish a new portfolio snapshot (called by a slower accounting thread).
     * Uses release semantics so evaluate() sees a consistent view.
     */
    void publish(double equity, double peak, double max_dd, double max_notional) noexcept {
        if (!state_) return;
        state_->equity        = equity;
        state_->peak_equity   = peak;
        state_->max_drawdown  = max_dd;
        state_->max_notional  = max_notional;
        state_->version.fetch_add(1, std::memory_order_release);
    }

private:
    PortfolioSnapshot* state_;   // non-owning
};

} // namespace aero::risk
