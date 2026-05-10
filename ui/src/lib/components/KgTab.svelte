<script>
  import { api } from '../api.js';

  let { palaceId } = $props();

  let subject = $state('');
  let triples = $state([]);
  let loading = $state(false);
  let error = $state(null);

  let assertSubject = $state('');
  let assertPredicate = $state('');
  let assertObject = $state('');
  let assertConfidence = $state(1.0);
  let assertProvenance = $state('');
  let asserting = $state(false);

  async function run() {
    if (!subject.trim()) return;
    loading = true;
    error = null;
    try {
      triples = await api.kgQuery(palaceId, subject.trim());
    } catch (e) {
      error = e.message;
    } finally {
      loading = false;
    }
  }

  async function assertTriple() {
    if (!assertSubject.trim() || !assertPredicate.trim() || !assertObject.trim()) return;
    asserting = true;
    error = null;
    try {
      await api.kgAssert(palaceId, {
        subject: assertSubject.trim(),
        predicate: assertPredicate.trim(),
        object: assertObject.trim(),
        confidence: Number(assertConfidence),
        provenance: assertProvenance.trim() || null
      });
      assertSubject = '';
      assertPredicate = '';
      assertObject = '';
      assertProvenance = '';
      if (subject) await run();
    } catch (e) {
      error = e.message;
    } finally {
      asserting = false;
    }
  }
</script>

<div class="grid">
  <div class="card">
    <div class="card-header">Query active triples</div>
    <div class="card-body">
      <div class="form-group">
        <label class="form-label" for="subj">Subject</label>
        <input
          id="subj"
          class="input"
          placeholder="e.g. alice"
          bind:value={subject}
          onkeydown={(e) => e.key === 'Enter' && run()}
        />
      </div>
      <button class="btn btn-primary" disabled={loading || !subject.trim()} onclick={run}>
        {loading ? 'Querying…' : 'Query'}
      </button>
      {#if error}<div class="badge badge-danger mt-3">{error}</div>{/if}

      {#if triples.length > 0}
        <table class="table mt-4">
          <thead>
            <tr>
              <th>Predicate</th>
              <th>Object</th>
              <th>Confidence</th>
              <th>Valid from</th>
            </tr>
          </thead>
          <tbody>
            {#each triples as t}
              <tr>
                <td class="text-mono text-sm">{t.predicate}</td>
                <td class="text-mono text-sm">{t.object}</td>
                <td>{t.confidence.toFixed(2)}</td>
                <td class="text-muted text-xs">{new Date(t.valid_from).toLocaleString()}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {:else if subject && !loading}
        <div class="empty">No active triples for this subject.</div>
      {/if}
    </div>
  </div>

  <div class="card">
    <div class="card-header">Assert triple</div>
    <div class="card-body">
      <div class="form-group">
        <label class="form-label" for="a-subj">Subject</label>
        <input id="a-subj" class="input" bind:value={assertSubject} />
      </div>
      <div class="form-group">
        <label class="form-label" for="a-pred">Predicate</label>
        <input id="a-pred" class="input" bind:value={assertPredicate} />
      </div>
      <div class="form-group">
        <label class="form-label" for="a-obj">Object</label>
        <input id="a-obj" class="input" bind:value={assertObject} />
      </div>
      <div class="form-group">
        <label class="form-label" for="a-conf">Confidence: {Number(assertConfidence).toFixed(2)}</label>
        <input id="a-conf" type="range" min="0" max="1" step="0.05" bind:value={assertConfidence} />
      </div>
      <div class="form-group">
        <label class="form-label" for="a-prov">Provenance (optional)</label>
        <input id="a-prov" class="input" bind:value={assertProvenance} />
      </div>
      <button
        class="btn btn-primary"
        disabled={asserting || !assertSubject.trim() || !assertPredicate.trim() || !assertObject.trim()}
        onclick={assertTriple}
      >
        {asserting ? 'Asserting…' : 'Assert'}
      </button>
    </div>
  </div>
</div>

<style>
  .grid {
    display: grid;
    grid-template-columns: 2fr 1fr;
    gap: var(--trusty-space-4);
  }
  @media (max-width: 1000px) {
    .grid {
      grid-template-columns: 1fr;
    }
  }
</style>
