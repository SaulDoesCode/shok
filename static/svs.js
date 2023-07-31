import http from "/http.js"
const svsapi = '/api/svs/'

export default (new Proxy({
    async get(m) { return await http.get(svsapi+m) },
    async set(m, v) { return await http.post(svsapi+m, v) },
    async delete(m) { return await http.del(svsapi+m) }
}, {
    async get(m, k) { return k in m ? m[k] : (await m.get(k)).value },
    async set(m, k, v) { return await m.set(k, v) },
    async deleteProperty(m, k) { return await m.delete(k) }
}))