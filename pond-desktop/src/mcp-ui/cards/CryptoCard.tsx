import { Activity, TrendingUp, TrendingDown } from "lucide-react";
import { registerMcpCard, type McpCardProps } from "../registry";

interface CoinData {
  symbol: string;
  name: string;
  price: string;
  change: number;
  sparkline?: number[];
}

function MiniSparkline({ data, positive }: { data: number[]; positive: boolean }) {
  if (data.length < 2) return null;

  const w = 64;
  const h = 26;
  const min = Math.min(...data);
  const max = Math.max(...data);
  const range = max - min || 1;
  const step = w / (data.length - 1);
  const points = data
    .map((v, i) => `${i * step},${h - ((v - min) / range) * h}`)
    .join(" ");

  return (
    <svg width={w} height={h} viewBox={`0 0 ${w} ${h}`}>
      <polyline
        points={points}
        fill="none"
        stroke={positive ? "#16A34A" : "#DC2626"}
        strokeWidth="1.75"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function normalizeCoins(data: Record<string, unknown>): CoinData[] {
  if (Array.isArray(data.coins)) {
    return data.coins as CoinData[];
  }
  // Single coin response — wrap it
  if (typeof data.symbol === "string") {
    return [
      {
        symbol: String(data.symbol),
        name: String(data.name ?? data.symbol),
        price: String(data.price ?? ""),
        change: Number(data.change ?? 0),
        sparkline: Array.isArray(data.sparkline) ? (data.sparkline as number[]) : undefined,
      },
    ];
  }
  return [];
}

function CryptoCard({ data, variant }: McpCardProps) {
  const coins = normalizeCoins(data);
  const isCompact = variant === "compact";
  const visibleCoins = coins.slice(0, isCompact ? 2 : coins.length);
  const hasAnySparkline = visibleCoins.some((c) => c.sparkline && c.sparkline.length >= 2);

  return (
    <div className="ui-card ui-crypto">
      {/* Header */}
      <div
        style={{
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          padding: "14px 16px 10px",
          gap: 8,
        }}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <Activity size={15} color="#10B981" />
          <span
            style={{
              fontSize: 13,
              fontWeight: 700,
              color: "#18181B",
              lineHeight: 1,
            }}
          >
            Crypto prices
          </span>
        </div>
        <div
          style={{
            display: "inline-flex",
            alignItems: "center",
            gap: 5,
            background: "#DCFCE7",
            color: "#15803D",
            fontSize: 11,
            fontWeight: 600,
            padding: "3px 8px",
            borderRadius: 9999,
            lineHeight: 1,
          }}
        >
          <span
            style={{
              width: 6,
              height: 6,
              borderRadius: "50%",
              background: "#16A34A",
              display: "inline-block",
              animation: "pulse-dot 1.8s ease-in-out infinite",
            }}
          />
          Live
        </div>
      </div>

      {/* Divider */}
      <div style={{ height: 1, background: "#F1F5F9" }} />

      {/* Coin rows */}
      <div>
        {visibleCoins.map((coin, idx) => {
          const positive = coin.change >= 0;
          const hasSparkline = coin.sparkline && coin.sparkline.length >= 2;
          const isLast = idx === visibleCoins.length - 1;

          return (
            <div
              key={coin.symbol}
              style={{
                display: "flex",
                alignItems: "center",
                gap: 10,
                padding: "10px 16px",
                borderBottom: isLast ? "none" : "1px solid #F8FAFC",
              }}
            >
              {/* Coin icon circle */}
              <div
                style={{
                  width: 36,
                  height: 36,
                  borderRadius: 10,
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                  flexShrink: 0,
                  background: positive ? "#F0FDF4" : "#FFF1F2",
                  border: `1px solid ${positive ? "#DCFCE7" : "#FFE4E6"}`,
                }}
              >
                <span
                  style={{
                    fontSize: 11,
                    fontWeight: 800,
                    color: positive ? "#16A34A" : "#DC2626",
                    lineHeight: 1,
                  }}
                >
                  {coin.symbol}
                </span>
              </div>

              {/* Coin name column */}
              <div
                style={{
                  flex: 1,
                  minWidth: 0,
                  display: "flex",
                  flexDirection: "column",
                  gap: 2,
                }}
              >
                <span
                  style={{
                    fontSize: 13,
                    fontWeight: 700,
                    color: "#18181B",
                    lineHeight: 1,
                  }}
                >
                  {coin.symbol}
                </span>
                <span
                  style={{
                    fontSize: 11,
                    fontWeight: 500,
                    color: "#94A3B8",
                    lineHeight: 1,
                  }}
                >
                  {coin.name}
                </span>
              </div>

              {/* Sparkline */}
              {hasAnySparkline && (
                <div style={{ flexShrink: 0, width: 64, height: 26 }}>
                  {hasSparkline && (
                    <MiniSparkline data={coin.sparkline!} positive={positive} />
                  )}
                </div>
              )}

              {/* Price column */}
              <div
                style={{
                  textAlign: "right",
                  flexShrink: 0,
                  display: "flex",
                  flexDirection: "column",
                  alignItems: "flex-end",
                  gap: 2,
                }}
              >
                <span
                  style={{
                    fontSize: 13,
                    fontWeight: 800,
                    color: "#18181B",
                    lineHeight: 1,
                  }}
                >
                  {coin.price}
                </span>
                <span
                  style={{
                    fontSize: 11,
                    fontWeight: 700,
                    color: positive ? "#16A34A" : "#DC2626",
                    lineHeight: 1,
                    display: "inline-flex",
                    alignItems: "center",
                    gap: 2,
                  }}
                >
                  {positive ? (
                    <TrendingUp size={11} />
                  ) : (
                    <TrendingDown size={11} />
                  )}
                  {positive ? "+" : ""}
                  {coin.change.toFixed(2)}%
                </span>
              </div>
            </div>
          );
        })}
      </div>

      {/* Pulse animation keyframes */}
      <style>{`
        @keyframes pulse-dot {
          0%, 100% { opacity: 1; transform: scale(1); }
          50% { opacity: 0.5; transform: scale(0.75); }
        }
      `}</style>
    </div>
  );
}

registerMcpCard({
  key: "crypto",
  label: "Crypto",
  icon: "Activity",
  toolPattern: /crypto|finance|price/,
  component: CryptoCard,
  mockTool: "giap-finance__get_crypto_prices",
  mockData: {
    coins: [
      {
        symbol: "BTC",
        name: "Bitcoin",
        price: "$67,842.30",
        change: 2.34,
        sparkline: [64200, 65100, 63800, 66400, 67100, 68200, 67842],
      },
      {
        symbol: "ETH",
        name: "Ethereum",
        price: "$3,456.12",
        change: -1.12,
        sparkline: [3520, 3580, 3510, 3480, 3470, 3490, 3456],
      },
      {
        symbol: "SOL",
        name: "Solana",
        price: "$172.50",
        change: 5.67,
        sparkline: [155, 160, 158, 165, 168, 170, 172],
      },
      {
        symbol: "ADA",
        name: "Cardano",
        price: "$0.48",
        change: -0.85,
        sparkline: [0.50, 0.49, 0.50, 0.49, 0.48, 0.49, 0.48],
      },
    ],
  },
});
