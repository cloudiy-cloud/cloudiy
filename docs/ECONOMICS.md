# Cloudiy — Modelo Econômico (working draft v0.1)

> **Status: rascunho de trabalho para decisão do dono.** Este documento existe
> porque a pergunta "como a rede incentiva todos os participantes de forma
> sustentável?" ainda não tinha resposta escrita — apontado por feedback externo
> (jul/2026) como o ponto mais fraco do projeto. As seções 1–2 descrevem o que o
> código **impõe hoje** (verificado contra a implementação); as seções 4–6 são
> propostas com DECISION POINTS. Nada aqui é promessa pública até decidido.

---

## 1. Fluxos de valor que o código impõe hoje

| Fluxo | Mecanismo | Onde no código |
|---|---|---|
| Consumer → escrow | USDC travado por job (`create_job`), ou por lease de VM (budget/rate) | `contracts/`, `crates/cloudiy/src/solana.rs`, `vm.rs` (lease + reaper) |
| Escrow → provider | `release`/`release_verified`: payout = preço − fee | contrato, 400 bps |
| Escrow → protocolo | **Fee de 4%** (400 bps) para `FEE_AUTHORITY` (constante de compile-time; hoje uma hot key) | `PROTOCOL_FEE_BPS`, `FEE_AUTHORITY` no contrato |
| Réplicas (RFC-0008) | Trustlessness custa N× o preço: um escrow por réplica; divergente = refund pós-deadline | `client.rs`, `sdk/quorum.rs` |
| Metering de VM | Prepaid: rate (USDC/h do node) × budget (escrow); reaper derruba VM exausta | `vm.rs` |

**O que NÃO é remunerado hoje** (os buracos):

1. **Directory nodes** — armazenam announcements, servem discovery, rodam
   probing de canary (custo real de CPU/rede/reputação) e não recebem nada.
2. **Oferta ociosa** — um provider online sem jobs não ganha nada. Não há
   emissão nem subsídio.
3. **Storage de volume** (RFC-0009) — sync/snapshot do estado não tem preço.
4. **O destino do fee** — 4% acumula numa hot key sem finalidade declarada.

## 2. A restrição de design (e por que ela é a cunha)

**Sem token próprio, por decisão.** A pesquisa competitiva (docs/COMPETITIVE.md)
mostra o padrão do setor: Nosana (NOS), io.net (IO) e Render (RNDR) usam token
para subsidiar oferta; Akash cobra ~20% de take rate em USDC. A literatura DePIN
do corpus Colosseum ("Why DePIN matters, and how to make it work") descreve o
flywheel clássico — token paga a oferta antes de existir demanda — e também o
seu cemitério: oferta mercenária que evapora quando a emissão cai, pressão de
venda estrutural, e demanda que nunca chega.

A aposta da Cloudiy é a inversa: **rede puxada por demanda, receita real em
USDC desde o primeiro job**. Isso é mais lento para bootstrapar oferta e mais
honesto como negócio. Este documento não propõe criar um token; propõe fazer o
fee de 4% trabalhar.

## 3. O princípio organizador: todo serviço da rede é um Resource pago

O manifesto de arquitetura já diz que *tudo é um Resource* e o pagamento é
x402. A consequência econômica natural: **discovery, storage e reputação não
são infraestrutura altruísta — são Resources vendidos pelos mesmos trilhos**
que vendem compute. Nenhum mecanismo novo, nenhum token: o protocolo já sabe
cobrar por serviço.

## 4. Modelos candidatos

### Modelo A — Split do fee de protocolo (routing fee)
O 4% deixa de acumular e passa a ser dividido no settlement:
`payout provider 96% · directory que intermediou a descoberta 1% · tesouraria 3%`
(números ilustrativos). Requer atribuição: o quote/announce carrega o id do
directory pelo qual o consumer descobriu o provider, e o split acontece no
release (contrato já suporta multi-payee via `create_job_split` / RFC-0004).
- ✅ Directory vira negócio proporcional ao volume que ele origina.
- ✅ Precedente direto no corpus: split de fee em stablecoin (validator rewards).
- ⚠️ Atribuição é jogável (directory pode se auto-atribuir); exige assinatura
  do announce path — extensão de protocolo (candidata a RFC-0011).

### Modelo B — Discovery como serviço x402 (preferido como direção)
O directory cobra micro-fee x402 pela query de discovery (consumer paga
~0.0001 USDC por `Providers`) e/ou pelo announce (provider paga listagem).
- ✅ Coerência total com o axioma "tudo é Resource"; zero mudança de contrato —
  x402 já existe no transporte.
- ✅ Qualquer um pode subir um directory e competir em preço/qualidade —
  descentralização por mercado, não por altruísmo.
- ⚠️ Fricção de micro-pagamento na primeira query (mitigável: primeiras N
  queries grátis, cobrança só de quem usa em volume — agentes).

### Modelo C — Bonds de disponibilidade (staking-lite em USDC)
Provider posta um bond pequeno em USDC; canary probes falhados (reputação já
existe, RFC-0006 §6) podem cortá-lo; o pool financia rebates de uptime.
- ✅ Skin-in-the-game sem token; melhora a qualidade da oferta.
- ⚠️ Slashing é o mecanismo mais delicado de acertar (falso positivo de canary
  = confisco injusto); complexidade alta para o estágio atual. **Adiar.**

### Oferta ociosa: a posição honesta
Não subsidiar. A rede é demand-led: provider ganha quando trabalha, e o
onboarding de 1 comando torna barato *entrar quando há demanda*. Isso é mais
fraco que io.net no curto prazo e mais são no longo — e deve ser dito assim,
inclusive no pitch.

## 5. Esboço de sustentabilidade (ordem de grandeza)

Um directory custa ~US$10–20/mês (VPS pequeno + banda). No Modelo B a
0.0001 USDC/query, breakeven ≈ 100–200 mil queries/mês — volume de agente, não
de humano; é exatamente o público-alvo. No Modelo A com 1% de routing fee,
breakeven ≈ US$1.000–2.000/mês de volume settled originado. Os dois fecham a
conta em escala pequena; o Modelo B fecha mais cedo.

## 6. DECISION POINTS (dono)

- **E1 — Destino do fee**: manter 4%? Split A (routing) ou tesouraria pura?
  O destino precisa migrar da hot key para multisig **antes** de mainnet
  (já é blocker no MAINNET-RUNBOOK; fee authority é compile-time).
- **E2 — Discovery paga (Modelo B)**: aprovar como direção e especificar em
  RFC-0011? Recomendação: **sim** — é a resposta estrutural ao "quem paga o
  directory" e não toca o contrato.
- **E3 — Storage de volume (RFC-0009)**: precificar o snapshot (USDC/GB·mês,
  pago ao operador do store) na v2 do volume, ou manter operador-arca-com-o-
  custo enquanto for beta?
- **E4 — Bonds (Modelo C)**: descartar por ora ou manter no roadmap? Recomendação:
  roadmap, revisitar pós-mainnet.
- **E5 — Réplicas no pricing**: o custo N× do quorum é o preço da trustlessness;
  exibi-lo como escolha explícita no CloudiyOS/SDK (1× confiado vs N× provado)?

## 7. O que isso responde ao feedback

O "modelo econômico 6,5/10" tinha razão: os fluxos existiam, a *rede* não.
Com E1+E2 decididos, cada participante tem receita: provider (jobs), directory
(discovery paga ou routing fee), operador de store (E3), protocolo (tesouraria
com finalidade: auditorias, relays, bounties). Sem token, sem emissão, sem
promessa que o código não imponha.
