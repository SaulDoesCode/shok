const api = {}

api.auth = async (moniker, pwd) => {
    const res = await fetch('/api/auth', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({moniker, pwd})
    })
    return await res.json()
}

api.search = async (query, page, k) => {
    const res = await fetch(`/api/search?q=${query}&p=${page}&k=${k}`)
    return await res.json()
}

api.search.write = async w => {
    /*
        const writ = {
            ts: undefined,
            title: wTitle.value.trim(),
            content: wContent.value.trim(),
            kind: 'post',
            tags: 'post ' + wTags.value.trim().replace('  ', ' ').replace('post', ''),
            public: wPublic.checked,
            price: Number(wPrice.value) || undefined,
            state: wState.value.trim() || '{}'
        }
        const res = await http[writ.ts == null ? 'put' : 'patch']('/api/search', writ)  
    */

    const res = await fetch(`/api/search`, {
        method: w.ts == null ? 'PUT' : 'PATCH',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(w)
    })
    return await res.json()
}

const ctjson = { 'Content-Type': 'application/json' }
const jsnPst = (v, o = {}) => ({method: 'POST', headers: {...ctjson}, body: JSON.stringify(v), ...o})
const svsA = '/api/svs/'
api.s = new Proxy(async (m, v, o = {}) => await (await fetch(svsA + m, v != null ? jsnPst(v, o) : v === null ? {method: 'DELETE', ...o} : o)).json(), {
    async get(m, k) { return await m(k) },
    async set(m, k, v) { return await m(k, v) },
    async deleteProperty(m, k) { return await m(k, null) }
})