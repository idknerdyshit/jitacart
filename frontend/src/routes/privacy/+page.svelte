<script lang="ts">
    const lastUpdated = '2026-05-08';
</script>

<svelte:head>
    <title>Privacy — JitaCart</title>
</svelte:head>

<article>
    <header>
        <h1>Privacy &amp; Data Handling</h1>
        <p class="muted">Last updated {lastUpdated}.</p>
    </header>

    <p>
        JitaCart is a logistics tool for EVE Online groups. It reads the minimum
        ESI data needed to match in-game contracts to shopping lists, and to
        display public market prices. The app never writes back to EVE.
    </p>

    <section>
        <h2>What we read from ESI</h2>
        <p>
            Each linked character grants only the scopes a feature requires.
            Scopes are requested progressively, not all at once, and can be
            revoked at any time on the
            <a href="https://community.eveonline.com/support/third-party-applications/" rel="noopener">
                EVE third-party-app management page
            </a>.
        </p>
        <ul>
            <li>
                <code>publicData</code> — always granted on first login. Lets us
                identify your character.
            </li>
            <li>
                <code>esi-contracts.read_character_contracts.v1</code> — for
                hauler-side contract tracking. Read-only.
            </li>
            <li>
                <code>esi-wallet.read_character_wallet.v1</code> — fallback
                price source if the hauler skips contracts. Read-only.
            </li>
            <li>
                <code>esi-markets.structure_markets.v1</code> — read public
                citadel market orders. Private citadels are explicitly out of
                scope.
            </li>
            <li>
                <code>esi-corporations.read_divisions.v1</code> — corp wallet
                division names. Granted only by corp ambassadors.
            </li>
            <li>
                <code>esi-wallet.read_corporation_wallets.v1</code> — corp
                wallet journals (requires the
                <code>Accountant</code> in-corp role).
            </li>
            <li>
                <code>esi-contracts.read_corporation_contracts.v1</code> — corp
                contracts (requires the <code>Contract Manager</code> in-corp
                role).
            </li>
        </ul>
    </section>

    <section>
        <h2>What we store</h2>
        <ul>
            <li>
                <strong>Account:</strong> a display name (your first character's
                EVE name) and the list of characters you've linked.
            </li>
            <li>
                <strong>Tokens:</strong> EVE refresh + access tokens, encrypted
                at rest with AES-256-GCM. The encryption key never appears in
                application logs and is rotated independently.
            </li>
            <li>
                <strong>App data:</strong> shopping lists you create or
                fulfill, contracts the worker matches to your lists, and
                reimbursements derived from those contracts.
            </li>
            <li>
                <strong>Market snapshots:</strong> aggregated best-sell / best-buy
                prices per market, per item. No personal data.
            </li>
        </ul>
    </section>

    <section>
        <h2>What we don't store</h2>
        <ul>
            <li>Your EVE password, ever.</li>
            <li>
                Plaintext refresh tokens — only encrypted ciphertext, decryptable
                only with the runtime key.
            </li>
            <li>Private citadel data, market orders, or wallet journals you
                haven't explicitly authorized.</li>
            <li>Personal data outside what ESI returns about your character.</li>
        </ul>
    </section>

    <section>
        <h2>Retention</h2>
        <ul>
            <li>
                <strong>Refresh tokens:</strong> kept until revoked by you (via
                EVE's third-party-app page) or until the character is deleted
                from the app.
            </li>
            <li>
                <strong>Lists, contracts, reimbursements:</strong> kept
                indefinitely so settlement history stays auditable. Archived
                lists can be hard-deleted by a group owner.
            </li>
            <li>
                <strong>Market snapshots:</strong> rolling 30-day window.
            </li>
            <li>
                <strong>Application logs:</strong> rotated by the host log
                driver; typical retention is 14 days.
            </li>
        </ul>
    </section>

    <section>
        <h2>Bot mitigation</h2>
        <p>
            The login page uses
            <a href="https://www.cloudflare.com/products/turnstile/" rel="noopener">Cloudflare Turnstile</a>
            to gate the creation of new accounts. Existing users with a valid
            session aren't challenged. Turnstile is privacy-friendly: it
            doesn't track users across sites and doesn't require solving a
            CAPTCHA in normal cases.
        </p>
    </section>

    <section>
        <h2>Export &amp; deletion</h2>
        <p>
            You can request a full JSON export of everything we hold about
            you at <code>GET /api/me/export</code> (logged in). To delete your
            account, leave or delete every group you're a member of and
            unlink every character on your profile page; the worker's
            cleanup pass removes orphan rows.
        </p>
    </section>

    <section>
        <h2>Source &amp; contact</h2>
        <p>
            JitaCart is open source. Issues, security reports, and PRs live
            on the project repository linked from the login page. For
            anything that shouldn't be public (security disclosures, account
            takeover, etc.), contact the operator listed in the user-agent
            header on every ESI request.
        </p>
    </section>
</article>

<style>
    article {
        line-height: 1.6;
    }
    h1 { font-size: 2rem; margin-bottom: 0.25rem; }
    h2 { font-size: 1.25rem; margin-top: 2rem; }
    .muted { color: #8b949e; font-size: 0.9rem; }
    code {
        background: #161b22;
        border: 1px solid #30363d;
        border-radius: 4px;
        padding: 0.05rem 0.35rem;
        font-size: 0.9em;
    }
    a { color: #58a6ff; }
    a:hover { text-decoration: underline; }
    ul { padding-left: 1.25rem; }
    li { margin-bottom: 0.4rem; }
</style>
